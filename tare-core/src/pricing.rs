//! Pricing as data. Loaded from `pricing.json` (shipped) or `pricing.fixture.toml`
//! (frozen goldens). Rates are integer micro-USD per million tokens. Never fetched.

use crate::model::{CacheClass, Provider};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRates {
    pub provider: String,
    pub model_id: String,
    #[serde(default = "default_tier")]
    pub tier: String,
    pub input_micro_per_mtok: i64,
    pub output_micro_per_mtok: i64,
    pub cache_read_micro_per_mtok: i64,
    pub cache_write_5m_micro_per_mtok: i64,
    pub cache_write_1h_micro_per_mtok: i64,
    /// Audio sub-class rates (multimodal). Default 0 → text-only models price audio at $0 and
    /// existing tables/goldens are byte-identical.
    #[serde(default)]
    pub audio_input_micro_per_mtok: i64,
    #[serde(default)]
    pub audio_output_micro_per_mtok: i64,
    /// Optional context-window pricing tiers: when the request's input exceeds a threshold the
    /// provider charges a higher (long-context) rate. Ordered evaluation picks the highest
    /// matching `over_tokens`. Empty for the common case — byte-identical for existing tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_tiers: Vec<ContextTier>,
    /// Effective date (`YYYY-MM-DD`) this row's rates took effect. Absent → inherits the table's
    /// `effective_date`. Multiple dated rows per (provider, model) let `lookup_on` reprice each
    /// historical run at its contemporaneous rate; single-edition tables are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_date: Option<String>,
}

/// A long-context rate override that applies when input tokens reach `over_tokens`. Each rate is
/// optional and falls back to the base row when absent (e.g. some models only bump the input rate).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextTier {
    pub over_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_micro_per_mtok: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_micro_per_mtok: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_micro_per_mtok: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_micro_per_mtok: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_micro_per_mtok: Option<i64>,
}

fn default_tier() -> String {
    "standard".to_string()
}

/// How to choose among dated pricing editions when repricing. Serde-friendly for a
/// `[pricing] reprice = "as-of" | "latest"` config / Settings toggle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PricingMode {
    /// Price each event at the edition effective on its own timestamp (the honest default).
    #[default]
    AsOf,
    /// Price everything at the newest edition ("what would this cost at today's prices?").
    Latest,
}

impl ModelRates {
    /// Rate for a given cache class. Reasoning is billed at the output rate.
    pub fn micro_per_mtok(&self, class: CacheClass) -> i64 {
        match class {
            CacheClass::Fresh => self.input_micro_per_mtok,
            CacheClass::CacheRead => self.cache_read_micro_per_mtok,
            CacheClass::CacheWrite5m => self.cache_write_5m_micro_per_mtok,
            CacheClass::CacheWrite1h => self.cache_write_1h_micro_per_mtok,
            CacheClass::Output => self.output_micro_per_mtok,
            CacheClass::Reasoning => self.output_micro_per_mtok,
        }
    }

    /// Resolve the effective rates for a request whose total input is `input_tokens`, applying the
    /// highest matching context tier (long-context pricing). Pure → attest/verify recompute the
    /// same cost from the same stored usage + table. Returns the base row when no tier applies.
    pub fn for_input(&self, input_tokens: u64) -> std::borrow::Cow<'_, ModelRates> {
        let Some(t) = self
            .context_tiers
            .iter()
            .filter(|t| input_tokens >= t.over_tokens)
            .max_by_key(|t| t.over_tokens)
        else {
            // Common path: no context tier applies — borrow, don't allocate.
            return std::borrow::Cow::Borrowed(self);
        };
        let mut r = self.clone();
        if let Some(v) = t.input_micro_per_mtok {
            r.input_micro_per_mtok = v;
        }
        if let Some(v) = t.output_micro_per_mtok {
            r.output_micro_per_mtok = v;
        }
        if let Some(v) = t.cache_read_micro_per_mtok {
            r.cache_read_micro_per_mtok = v;
        }
        if let Some(v) = t.cache_write_5m_micro_per_mtok {
            r.cache_write_5m_micro_per_mtok = v;
        }
        if let Some(v) = t.cache_write_1h_micro_per_mtok {
            r.cache_write_1h_micro_per_mtok = v;
        }
        r.context_tiers = Vec::new(); // resolved
        std::borrow::Cow::Owned(r)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingTable {
    pub version: String,
    pub effective_date: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(alias = "model")]
    pub models: Vec<ModelRates>,
    /// Lazily-built `(provider, model_id) -> models[i]` index so lookups are O(log n)
    /// instead of a linear scan per cause per step. Not serialized; not part of
    /// equality (it's a pure function of `models`).
    #[serde(skip)]
    index: OnceLock<BTreeMap<String, BTreeMap<String, usize>>>,
    /// User-supplied self-hosted cost overlay: `backend -> synthetic rate`, applied to
    /// ANY model on that backend when the table has no explicit row. Not serialized and not part of
    /// equality (it comes from `tare.toml`, not the pricing file); empty by default, so an
    /// un-configured table prices and compares byte-identically to before.
    #[serde(skip)]
    local_overlay: BTreeMap<String, ModelRates>,
    /// User per-model rate overrides (slice): `model_id -> rate` from `[pricing]` in
    /// tare.toml, checked FIRST in `lookup` so they win over the shipped table — a user can price an
    /// otherwise-unpriced model (GAP → estimate) or correct a stale rate. Not serialized, not part of
    /// equality; empty by default, so an un-configured table prices byte-identically to before.
    #[serde(skip)]
    model_overrides: BTreeMap<String, ModelRates>,
}

// Equality/Hash ignore the derived `index` cache — it's a function of `models`.
impl PartialEq for PricingTable {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.effective_date == other.effective_date
            && self.note == other.note
            && self.models == other.models
    }
}
impl Eq for PricingTable {}

/// Convert a per-token USD cost to integer micro-USD per million tokens: `$/tok × 1e6 tok/Mtok ×
/// 1e6 µ$/$ = ×1e12`. ALWAYS rounds (never truncates) so a rate like `3e-6` lands on `3_000_000`,
/// not `2_999_999`.
fn per_token_to_micro_mtok(cost_per_token: f64) -> Result<i64, String> {
    scaled_rate(cost_per_token, 1e12, "per-token rate")
}

/// Convert a per-MILLION-token USD cost (models.dev) to integer micro-USD/Mtok: `$/Mtok × 1e6
/// µ$/$ = ×1e6`, always rounded.
fn per_mtok_to_micro_mtok(cost_per_mtok: f64) -> Result<i64, String> {
    scaled_rate(cost_per_mtok, 1e6, "per-million-token rate")
}

fn scaled_rate(value: f64, scale: f64, kind: &str) -> Result<i64, String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{kind} must be finite and non-negative"));
    }
    let scaled = (value * scale).round();
    // `i64::MAX as f64` rounds up to 2^63, which is already outside i64. Using `>=`
    // avoids accepting that boundary and relying on Rust's saturating float cast.
    if !scaled.is_finite() || scaled >= i64::MAX as f64 {
        return Err(format!("{kind} is out of micro-USD range"));
    }
    Ok(scaled as i64)
}

/// Derive the 1-hour cache-write rate from the 5-minute rate using the exact 8/5 ratio.
fn cache_write_1h_rate(write_5m: i64) -> Result<i64, String> {
    let value = (i128::from(write_5m) * 8 + 2) / 5;
    i64::try_from(value).map_err(|_| "derived 1-hour cache rate is out of range".to_string())
}

/// Read a non-negative rate from a JSON object. Missing/null optional rates are zero; a present
/// malformed value is an error rather than silently turning a priced token class into a free one.
fn f64_field(v: &serde_json::Value, key: &str) -> Result<f64, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(0.0),
        Some(serde_json::Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| format!("field {key:?} is outside the supported numeric range")),
        Some(serde_json::Value::String(s)) => s
            .parse::<f64>()
            .map_err(|_| format!("field {key:?} must be a number or decimal string")),
        Some(_) => Err(format!("field {key:?} must be a number or decimal string")),
    }
}

impl PricingTable {
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        let table: Self = serde_json::from_str(s).map_err(|e| format!("pricing json: {e}"))?;
        table.validate().map_err(|e| format!("pricing json: {e}"))
    }

    /// Parse LiteLLM's `model_prices_and_context_window.json` (a map of model-id → per-token costs)
    /// into a DATED pricing edition. Per-token → micro/Mtok via `×1e12, round()`.
    /// Cache writes: LiteLLM's `cache_creation_input_token_cost` is the 5m tier; the 1h tier uses
    /// `cache_creation_input_token_cost_above_1hr` when present, else the documented Anthropic
    /// 2.0/1.25 = 1.6× ratio off the 5m rate. Entries without `input_cost_per_token` /
    /// `litellm_provider` (meta rows like `sample_spec`) are skipped. `version` is stamped for
    /// provenance; `effective_date` is supplied by the (clock-owning) caller — the core never reads
    /// a clock. Models are sorted by (provider, model_id) for deterministic output.
    pub fn from_litellm_json(s: &str, effective_date: &str) -> Result<Self, String> {
        validate_pricing_date("effective_date", effective_date)?;
        let root: serde_json::Value =
            serde_json::from_str(s).map_err(|e| format!("litellm json: {e}"))?;
        let obj = root
            .as_object()
            .ok_or("litellm json: expected a top-level object")?;
        let mut models: Vec<ModelRates> = Vec::new();
        for (id, spec) in obj {
            if !spec.is_object() {
                continue;
            }
            let provider = match spec.get("litellm_provider") {
                None | Some(serde_json::Value::Null) => "",
                Some(serde_json::Value::String(provider)) => provider,
                Some(_) => {
                    return Err(format!(
                        "litellm model {id:?}: field \"litellm_provider\" must be a string"
                    ));
                }
            };
            // Require a real priced model row (skips `sample_spec` + non-model meta entries).
            if provider.is_empty() || spec.get("input_cost_per_token").is_none() {
                continue;
            }
            let w5 = per_token_to_micro_mtok(f64_field(spec, "cache_creation_input_token_cost")?)?;
            let w1 = {
                let explicit = f64_field(spec, "cache_creation_input_token_cost_above_1hr")?;
                if explicit > 0.0 {
                    per_token_to_micro_mtok(explicit)?
                } else if w5 > 0 {
                    // Anthropic ratio: 1h = 2×input, 5m = 1.25×input ⇒ 1h = 5m × 1.6.
                    cache_write_1h_rate(w5)?
                } else {
                    0
                }
            };
            // Long-context (above-200k) tier: LiteLLM carries `*_above_200k_tokens`
            // rates for models that price large prompts higher (Anthropic 1M-context, Gemini). Parse
            // them into a ContextTier so `for_input` applies the right rate — exact, from stored
            // counts. A field is "present" iff it converts to a positive micro-rate.
            let above = |k: &str| per_token_to_micro_mtok(f64_field(spec, k)?);
            let ti = above("input_cost_per_token_above_200k_tokens")?;
            let to = above("output_cost_per_token_above_200k_tokens")?;
            let tcr = above("cache_read_input_token_cost_above_200k_tokens")?;
            let tw5 = above("cache_creation_input_token_cost_above_200k_tokens")?;
            let context_tiers = if ti > 0 || to > 0 || tcr > 0 || tw5 > 0 {
                vec![ContextTier {
                    over_tokens: 200_000,
                    input_micro_per_mtok: (ti > 0).then_some(ti),
                    output_micro_per_mtok: (to > 0).then_some(to),
                    cache_read_micro_per_mtok: (tcr > 0).then_some(tcr),
                    cache_write_5m_micro_per_mtok: (tw5 > 0).then_some(tw5),
                    // Mirror the base 1h = 1.6×5m ratio when only the 5m long-context rate is given.
                    cache_write_1h_micro_per_mtok: if tw5 > 0 {
                        Some(cache_write_1h_rate(tw5)?)
                    } else {
                        None
                    },
                }]
            } else {
                Vec::new()
            };
            // LiteLLM sometimes prefixes the key with `provider/`; store the bare model id.
            let model_id = id.rsplit('/').next().unwrap_or(id).to_string();
            models.push(ModelRates {
                provider: provider.to_string(),
                model_id,
                tier: default_tier(),
                input_micro_per_mtok: per_token_to_micro_mtok(f64_field(
                    spec,
                    "input_cost_per_token",
                )?)?,
                output_micro_per_mtok: per_token_to_micro_mtok(f64_field(
                    spec,
                    "output_cost_per_token",
                )?)?,
                cache_read_micro_per_mtok: per_token_to_micro_mtok(f64_field(
                    spec,
                    "cache_read_input_token_cost",
                )?)?,
                cache_write_5m_micro_per_mtok: w5,
                cache_write_1h_micro_per_mtok: w1,
                audio_input_micro_per_mtok: 0,
                audio_output_micro_per_mtok: 0,
                context_tiers,
                effective_date: None,
            });
        }
        models.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then(a.model_id.cmp(&b.model_id))
        });
        Self::from_models(format!("litellm-{effective_date}"), effective_date, models)
    }

    /// Build a table from already-parsed rows (rebuilds the internal lookup index).
    /// Parse models.dev's `api.json` (a provider registry: `{provider: {models: {id: {cost:{input,
    /// output,cache_read,cache_write}}}}}`, costs in per-MILLION-token USD) into a dated edition
    /// (secondary source). Per-Mtok → micro/Mtok via `×1e6, round()`. When a model id
    /// is namespaced (`anthropic/claude-…`) the namespace is the provider; otherwise the registry
    /// key is. 1h cache write derives from the 5m `cache_write` at the Anthropic 1.6× ratio.
    pub fn from_modelsdev_json(s: &str, effective_date: &str) -> Result<Self, String> {
        validate_pricing_date("effective_date", effective_date)?;
        let root: serde_json::Value =
            serde_json::from_str(s).map_err(|e| format!("models.dev json: {e}"))?;
        let providers = root
            .as_object()
            .ok_or("models.dev json: expected a top-level object")?;
        let mut models: Vec<ModelRates> = Vec::new();
        for (registry_key, pobj) in providers {
            let Some(mmap) = pobj.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (mid, spec) in mmap {
                let Some(cost) = spec.get("cost") else {
                    continue;
                };
                if cost.get("input").is_none() {
                    continue;
                }
                // `anthropic/claude-…` → (anthropic, claude-…); bare id → registry provider.
                let (provider, model_id) = match mid.split_once('/') {
                    Some((p, m)) => (p.to_string(), m.to_string()),
                    None => (registry_key.clone(), mid.clone()),
                };
                let w5 = per_mtok_to_micro_mtok(f64_field(cost, "cache_write")?)?;
                let w1 = if w5 > 0 { cache_write_1h_rate(w5)? } else { 0 };
                models.push(ModelRates {
                    provider,
                    model_id,
                    tier: default_tier(),
                    input_micro_per_mtok: per_mtok_to_micro_mtok(f64_field(cost, "input")?)?,
                    output_micro_per_mtok: per_mtok_to_micro_mtok(f64_field(cost, "output")?)?,
                    cache_read_micro_per_mtok: per_mtok_to_micro_mtok(f64_field(
                        cost,
                        "cache_read",
                    )?)?,
                    cache_write_5m_micro_per_mtok: w5,
                    cache_write_1h_micro_per_mtok: w1,
                    audio_input_micro_per_mtok: 0,
                    audio_output_micro_per_mtok: 0,
                    context_tiers: Vec::new(),
                    effective_date: None,
                });
            }
        }
        models.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then(a.model_id.cmp(&b.model_id))
        });
        Self::from_models(
            format!("modelsdev-{effective_date}"),
            effective_date,
            models,
        )
    }

    /// Merge two editions, PREFERRING `self`'s rows: LiteLLM/first-party pricing is
    /// authoritative, and `secondary` (models.dev) only FILLS `(provider, model_id)` gaps — never
    /// overriding a primary rate. Deterministic (sorted by provider, model_id); keeps `self`'s
    /// version + effective_date.
    pub fn merge_prefer(&self, secondary: &PricingTable) -> Result<PricingTable, String> {
        use std::collections::BTreeSet;
        let have: BTreeSet<(String, String)> = self
            .models
            .iter()
            .map(|r| (r.provider.clone(), r.model_id.clone()))
            .collect();
        let mut merged = self.models.clone();
        for r in &secondary.models {
            if !have.contains(&(r.provider.clone(), r.model_id.clone())) {
                merged.push(r.clone());
            }
        }
        merged.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then(a.model_id.cmp(&b.model_id))
        });
        Self::from_models(self.version.clone(), &self.effective_date, merged)
    }

    pub fn from_models(
        version: String,
        effective_date: &str,
        models: Vec<ModelRates>,
    ) -> Result<Self, String> {
        let json = serde_json::json!({
            "version": version,
            "effective_date": effective_date,
            "model": models,
        });
        Self::from_json_str(&json.to_string())
    }

    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        let table: Self = toml::from_str(s).map_err(|e| format!("pricing toml: {e}"))?;
        table.validate().map_err(|e| format!("pricing toml: {e}"))
    }

    fn validate(self) -> Result<Self, String> {
        validate_nonempty_pricing_field("version", &self.version)?;
        validate_pricing_date("effective_date", &self.effective_date)?;
        let mut editions = BTreeSet::new();
        for (index, model) in self.models.iter().enumerate() {
            let prefix = format!("models[{index}]");
            validate_nonempty_pricing_field(&format!("{prefix}.provider"), &model.provider)?;
            validate_nonempty_pricing_field(&format!("{prefix}.model_id"), &model.model_id)?;
            validate_nonempty_pricing_field(&format!("{prefix}.tier"), &model.tier)?;
            if model.tier != "standard" {
                return Err(format!(
                    "{prefix}.tier {:?} is unsupported; expected \"standard\"",
                    model.tier
                ));
            }
            for (field, rate) in [
                ("input_micro_per_mtok", model.input_micro_per_mtok),
                ("output_micro_per_mtok", model.output_micro_per_mtok),
                ("cache_read_micro_per_mtok", model.cache_read_micro_per_mtok),
                (
                    "cache_write_5m_micro_per_mtok",
                    model.cache_write_5m_micro_per_mtok,
                ),
                (
                    "cache_write_1h_micro_per_mtok",
                    model.cache_write_1h_micro_per_mtok,
                ),
                (
                    "audio_input_micro_per_mtok",
                    model.audio_input_micro_per_mtok,
                ),
                (
                    "audio_output_micro_per_mtok",
                    model.audio_output_micro_per_mtok,
                ),
            ] {
                if rate < 0 {
                    return Err(format!("{prefix}.{field} must be non-negative"));
                }
            }
            let effective_date = model
                .effective_date
                .as_deref()
                .unwrap_or(&self.effective_date);
            validate_pricing_date(&format!("{prefix}.effective_date"), effective_date)?;
            if !editions.insert((
                model.provider.as_str(),
                model.model_id.as_str(),
                effective_date,
            )) {
                return Err(format!(
                    "duplicate pricing edition for {}/{} on {}",
                    model.provider, model.model_id, effective_date
                ));
            }

            let mut thresholds = BTreeSet::new();
            for (tier_index, tier) in model.context_tiers.iter().enumerate() {
                let tier_prefix = format!("{prefix}.context_tiers[{tier_index}]");
                if tier.over_tokens == 0 {
                    return Err(format!(
                        "{tier_prefix}.over_tokens must be greater than zero"
                    ));
                }
                if !thresholds.insert(tier.over_tokens) {
                    return Err(format!(
                        "{prefix}.context_tiers has duplicate threshold {}",
                        tier.over_tokens
                    ));
                }
                let tier_rates = [
                    ("input_micro_per_mtok", tier.input_micro_per_mtok),
                    ("output_micro_per_mtok", tier.output_micro_per_mtok),
                    ("cache_read_micro_per_mtok", tier.cache_read_micro_per_mtok),
                    (
                        "cache_write_5m_micro_per_mtok",
                        tier.cache_write_5m_micro_per_mtok,
                    ),
                    (
                        "cache_write_1h_micro_per_mtok",
                        tier.cache_write_1h_micro_per_mtok,
                    ),
                ];
                if tier_rates.iter().all(|(_, rate)| rate.is_none()) {
                    return Err(format!("{tier_prefix} must override at least one rate"));
                }
                for (field, rate) in tier_rates {
                    if rate.is_some_and(|rate| rate < 0) {
                        return Err(format!("{tier_prefix}.{field} must be non-negative"));
                    }
                }
            }
        }
        Ok(self)
    }

    /// Date-aware lookup: the rate row in effect on `date` — the latest row whose effective
    /// date is on-or-before `date` (a row's own `effective_date`, else the table's). Falls back to
    /// the earliest row when `date` precedes all editions. For a single-edition table this equals
    /// `lookup`. Used by the trend so re-running it after a price change reprices history correctly.
    /// Repricing mode: how to choose among dated editions. `AsOf` prices each event at
    /// the edition contemporaneous with its OWN timestamp (so a session resumed across a price change
    /// keeps old turns at the old rate, new turns at the new — the honest default); `Latest` prices
    /// everything at the newest edition (ccusage's `calculate` behavior — "what would this cost
    /// today?"). Both go through `lookup_on`, so multi-edition tables reprice deterministically and
    /// single-edition tables behave identically under either mode.
    pub fn lookup_mode(
        &self,
        provider: Provider,
        vendor: Option<&str>,
        model_id: &str,
        event_date: &str,
        mode: PricingMode,
    ) -> Option<&ModelRates> {
        match mode {
            PricingMode::AsOf => self.lookup_on(provider, vendor, model_id, event_date),
            // A sentinel later than any real YYYY-MM-DD → as-of selects the newest edition.
            PricingMode::Latest => self.lookup_on(provider, vendor, model_id, "9999-12-31"),
        }
    }

    pub fn lookup_on(
        &self,
        provider: Provider,
        vendor: Option<&str>,
        model_id: &str,
        date: &str,
    ) -> Option<&ModelRates> {
        let table_date = self.effective_date.as_str();
        let eff = |r: &ModelRates| {
            r.effective_date
                .as_deref()
                .unwrap_or(table_date)
                .to_string()
        };
        let pkey = provider.pricing_key(vendor);
        let mut rows: Vec<&ModelRates> = self
            .models
            .iter()
            .filter(|r| r.provider == pkey && r.model_id == model_id)
            .collect();
        if rows.is_empty() {
            return self.lookup(provider, vendor, model_id); // warns once, returns None
        }
        rows.sort_by_key(|r| eff(r));
        rows.iter()
            .rev()
            .find(|r| eff(r).as_str() <= date)
            .or_else(|| rows.first())
            .copied()
    }

    /// A single-edition view of this table as of `date`: for each (provider, model_id),
    /// keep the one row in effect on `date` — the latest `effective_date <= date`, else the earliest
    /// (matching `lookup_on`) — restamped to `date`. `build_report(runs, &table.as_of(d))` then reprices
    /// every step at `d`'s rates: a pricing-snapshot. A single-edition table yields an equivalent
    /// single-edition view (same rates, so same cost). Local overlay + per-model overrides carry over so
    /// repricing stays consistent with the source table.
    pub fn as_of(&self, date: &str) -> PricingTable {
        let table_date = self.effective_date.as_str();
        let eff = |r: &ModelRates| {
            r.effective_date
                .as_deref()
                .unwrap_or(table_date)
                .to_string()
        };
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let mut chosen: Vec<ModelRates> = Vec::new();
        for r in &self.models {
            // First-seen (provider, model_id) only — deterministic, one row per model in the view.
            if !seen.insert((r.provider.clone(), r.model_id.clone())) {
                continue;
            }
            let mut rows: Vec<&ModelRates> = self
                .models
                .iter()
                .filter(|x| x.provider == r.provider && x.model_id == r.model_id)
                .collect();
            rows.sort_by_key(|x| eff(x));
            if let Some(row) = rows
                .iter()
                .rev()
                .find(|x| eff(x).as_str() <= date)
                .or_else(|| rows.first())
                .copied()
            {
                let mut row = row.clone();
                row.effective_date = Some(date.to_string());
                chosen.push(row);
            }
        }
        // from_models round-trips valid rows, so the error arm is unreachable in practice; fall back to
        // the full table rather than panic if it ever isn't.
        let mut out =
            PricingTable::from_models(format!("{}-asof-{date}", self.version), date, chosen)
                .unwrap_or_else(|_| self.clone());
        out.local_overlay = self.local_overlay.clone();
        out.model_overrides = self.model_overrides.clone();
        out
    }

    /// Merge two price maps, preferring THIS table where both define a model (`merge_prefer`).
    /// Use it to fold a broad-but-secondary source (models.dev) UNDER a first-party/authoritative one
    /// (LiteLLM): every model self already prices is kept verbatim; `fallback` only contributes models
    /// self is missing (e.g. Anthropic long-context rows LiteLLM has that models.dev lacks stay from
    /// LiteLLM). Keys on (provider, model_id). Self's version/effective_date are retained. Deterministic
    /// (fallback additions sorted by key), pure.
    pub fn merged_with(&self, fallback: &PricingTable) -> PricingTable {
        use std::collections::BTreeMap;
        let key = |r: &ModelRates| (r.provider.clone(), r.model_id.clone());
        let mine: std::collections::BTreeSet<(String, String)> =
            self.models.iter().map(key).collect();
        let mut extra: BTreeMap<(String, String), ModelRates> = BTreeMap::new();
        for r in &fallback.models {
            let k = key(r);
            if !mine.contains(&k) {
                extra.insert(k, r.clone()); // last fallback row for a key wins; then sorted by key
            }
        }
        let mut models = self.models.clone();
        models.extend(extra.into_values());
        let mut out = PricingTable::from_models(self.version.clone(), &self.effective_date, models)
            .unwrap_or_else(|_| self.clone());
        out.local_overlay = self.local_overlay.clone();
        out.model_overrides = self.model_overrides.clone();
        out
    }

    /// The newest edition date present (max row `effective_date`, else the table's `effective_date`).
    pub fn newest_edition_date(&self) -> String {
        self.models
            .iter()
            .filter_map(|r| r.effective_date.clone())
            .chain(std::iter::once(self.effective_date.clone()))
            .max()
            .unwrap_or_else(|| self.effective_date.clone())
    }

    /// Every distinct dated edition present, ascending: each row `effective_date` plus
    /// the table's own `effective_date`, deduped + sorted. These are exactly the snapshots a
    /// pricing-snapshot CostExperiment axis can reprice against, so a UI can auto-propose them.
    pub fn edition_dates(&self) -> Vec<String> {
        let mut dates: Vec<String> = self
            .models
            .iter()
            .filter_map(|r| r.effective_date.clone())
            .chain(std::iter::once(self.effective_date.clone()))
            .collect();
        dates.sort();
        dates.dedup();
        dates
    }

    /// The table to cost against under a repricing mode. `AsOf` keeps the table as-is
    /// (single-edition: exact; multi-edition: the report path's default per-model lookup — true
    /// per-run AsOf awaits `RunRecord.day` plumbing). `Latest` collapses to the newest edition via
    /// [`as_of`], so all history reprices at today's rates. A single-edition table is unaffected by
    /// either mode (one edition).
    pub fn for_mode(&self, mode: PricingMode) -> PricingTable {
        match mode {
            PricingMode::AsOf => self.clone(),
            PricingMode::Latest => self.as_of(&self.newest_edition_date()),
        }
    }

    /// True iff some model carries more than one dated edition row — i.e. repricing-by-date can
    /// actually change a rate. When false (the common single-edition table: the bundled table + any
    /// one-edition models.dev/LiteLLM import), callers should pass an EMPTY day-map so lookups take
    /// the O(log n) index path instead of `row_on`'s per-lookup linear scan+sort.
    pub fn is_multi_edition(&self) -> bool {
        let mut seen = std::collections::HashSet::new();
        for r in &self.models {
            if !seen.insert((r.provider.as_str(), r.model_id.as_str())) {
                return true; // a repeated (provider, model_id) ⇒ >1 edition
            }
        }
        false
    }

    /// Fold the user's config into this base table: the self-hosted cost overlay
    /// (`[providers]`), per-model rate overrides + repricing mode (`[pricing]`). This is the ONE
    /// assembly the CLI (`load_pricing`) and the desktop (`pricing()`) both call, so their dollar
    /// figures can never drift — previously the desktop applied only the overlay, silently dropping
    /// overrides + reprice mode. All layers empty/default → the table is unchanged.
    pub fn with_config(mut self, cfg: &crate::config::TareConfig) -> PricingTable {
        self.set_local_overlay(&cfg.providers.local_overlay);
        self.set_model_overrides(&cfg.pricing.overrides);
        self.for_mode(cfg.pricing.reprice)
    }

    /// The lazily-built `provider -> model_id -> first-row index` (first row wins per key). Nested
    /// so a lookup probes by `&str` (via `String: Borrow<str>`) with no per-call key allocation
    /// — this is the most-called function in the cost path.
    fn index_ref(&self) -> &BTreeMap<String, BTreeMap<String, usize>> {
        self.index.get_or_init(|| {
            let mut m: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            for (i, r) in self.models.iter().enumerate() {
                m.entry(r.provider.clone())
                    .or_default()
                    .entry(r.model_id.clone())
                    .or_insert(i);
            }
            m
        })
    }

    /// The `(pkey, model_id)` row in effect on `date` (latest `effective_date <= date`, else the
    /// earliest) — the date-aware analogue of the first-row index pick. `None` when no row matches.
    fn row_on(&self, pkey: &str, model_id: &str, date: &str) -> Option<&ModelRates> {
        let table_date = self.effective_date.clone();
        // Owned String (not &str) so the closure's return lifetime is unambiguous.
        let eff = |r: &ModelRates| {
            r.effective_date
                .clone()
                .unwrap_or_else(|| table_date.clone())
        };
        let mut rows: Vec<&ModelRates> = self
            .models
            .iter()
            .filter(|r| r.provider == pkey && r.model_id == model_id)
            .collect();
        rows.sort_by_key(|r| eff(r));
        rows.iter()
            .rev()
            .find(|r| eff(r).as_str() <= date)
            .or_else(|| rows.first())
            .copied()
    }

    /// Look up rates by provider + model, defaulting to the `standard` tier. Builds (once)
    /// and consults an index. On a miss, emits a one-time diagnostic so an unpriced model
    /// surfaces as "estimated, zero-cost" rather than silently vanishing from the totals.
    pub fn lookup(
        &self,
        provider: Provider,
        vendor: Option<&str>,
        model_id: &str,
    ) -> Option<&ModelRates> {
        self.lookup_as_of(provider, vendor, model_id, None)
    }

    /// Date-aware lookup: identical to [`lookup`] but, when `on` is `Some(date)`, the
    /// table-row pick selects the edition in effect on `date` (via [`row_on`]) instead of the
    /// first-row index — so a multi-edition table reprices each run at its contemporaneous rate
    /// (honest AsOf). **The `None` path is byte-identical to the previous `lookup`.** Crucially the
    /// SAME chain runs in both modes: per-model override wins first (date-independent — a user's
    /// explicit current rate), then the table row, then the self-hosted overlay, then the dated-alias
    /// suffix strip — so date-aware pricing never silently drops overrides/overlay/suffix (the bug a
    /// naive `lookup_on` in the hot path would introduce).
    pub fn lookup_as_of(
        &self,
        provider: Provider,
        vendor: Option<&str>,
        model_id: &str,
        on: Option<&str>,
    ) -> Option<&ModelRates> {
        // 1. User per-model override wins (empty by default → no effect).
        if let Some(row) = self.model_overrides.get(model_id) {
            return Some(row);
        }
        // The provider dimension is the vendor label for OpenAI-compatible endpoints.
        let pkey = provider.pricing_key(vendor);
        // 2. Table row: date-select when `on` is set, else the indexed first row (prior behavior).
        let hit = match on {
            None => self
                .index_ref()
                .get(pkey.as_ref())
                .and_then(|by_model| by_model.get(model_id))
                .map(|&i| &self.models[i]),
            Some(date) => self.row_on(pkey.as_ref(), model_id, date),
        };
        if hit.is_some() {
            return hit;
        }
        // 3. Self-hosted overlay: a per-backend rate prices ANY model on that backend.
        if provider == Provider::Local {
            if let Some(row) = vendor.and_then(|v| self.local_overlay.get(v)) {
                return Some(row);
            }
        }
        // 4. Dated-alias suffix strip: "claude-haiku-4-5-20251001" → "claude-haiku-4-5"
        // on a genuine miss. One retry, no recursion; honors the same `on` date semantics.
        if let Some(base) = strip_date_suffix(model_id) {
            // A user override keyed on the base id must also apply to its dated aliases
            // — mirror step 1's precedence so an intentional correction isn't silently bypassed for
            // the dated model id the capture actually records.
            if let Some(row) = self.model_overrides.get(base) {
                return Some(row);
            }
            let hit = match on {
                None => self
                    .index_ref()
                    .get(pkey.as_ref())
                    .and_then(|by_model| by_model.get(base))
                    .map(|&i| &self.models[i]),
                Some(date) => self.row_on(pkey.as_ref(), base, date),
            };
            if hit.is_some() {
                return hit;
            }
        }
        // 5. Bedrock/Vertex decoration strip: `anthropic.claude-haiku-4-5-20251001-v1:0`
        // → `claude-haiku-4-5-20251001`, which then prices directly or via step 4's date strip. Delegate
        // to a single recursive retry with the canonical id so overrides + date-alias logic all apply;
        // that call owns the miss-warning, so we don't warn twice. Only KNOWN prefixes are stripped, so
        // an undecorated id (incl. dotted names like `gpt-4.1`) returns None here and falls through.
        if let Some(base) = strip_bedrock_decoration(model_id) {
            return self.lookup_as_of(provider, vendor, base, on);
        }
        warn_missing_pricing(provider, vendor, model_id);
        None
    }

    /// Install a user-supplied self-hosted cost overlay from config: each backend with a
    /// resolvable rate becomes a synthetic per-Mtok row applied to every model on that backend
    /// (input == output; self-hosted compute doesn't split the two). Backends without a rate are
    /// skipped. Replaces any prior overlay. Does not touch the priced `models` or the index, so
    /// tables with no overlay are unaffected.
    pub fn set_local_overlay(&mut self, overlays: &[crate::config::LocalOverlay]) {
        self.local_overlay = overlays
            .iter()
            .filter_map(|o| {
                let micro = o.micro_per_mtok()?;
                Some((
                    o.backend.clone(),
                    ModelRates {
                        provider: format!("local:{}", o.backend),
                        model_id: "*".to_string(),
                        tier: default_tier(),
                        input_micro_per_mtok: micro,
                        output_micro_per_mtok: micro,
                        cache_read_micro_per_mtok: 0,
                        cache_write_5m_micro_per_mtok: 0,
                        cache_write_1h_micro_per_mtok: 0,
                        audio_input_micro_per_mtok: 0,
                        audio_output_micro_per_mtok: 0,
                        context_tiers: Vec::new(),
                        effective_date: None,
                    },
                ))
            })
            .collect();
    }

    /// Install user per-model rate overrides (slice) from `[pricing]` in tare.toml.
    /// USD/Mtok → integer µ$/Mtok (rounded, matching the loader). Replaces any prior overrides;
    /// empty → pricing is unchanged.
    ///
    /// Cache economics: if a shipped row ALREADY prices this model, the override
    /// INHERITS that row's real cache rates (e.g. Anthropic cache_read 0.1×, cache_write 1.25×/2× of
    /// input) — so overriding a priced model merely to correct input/output no longer silently
    /// collapses its cache reads to 1× (a ~10× over-estimate on the dominant Claude token class) or
    /// its writes to 1×. Only a genuinely-UNPRICED model (no shipped row — e.g. a local backend with
    /// no cache tiers) falls back to cache ≈ input, which is defensible there. An explicit
    /// `cache_read_usd_per_mtok` in the override always wins.
    pub fn set_model_overrides(&mut self, overrides: &[crate::config::ModelOverride]) {
        let built: std::collections::BTreeMap<String, ModelRates> = overrides
            .iter()
            .filter_map(|o| {
                // Reject non-finite or negative override rates: a NaN would cast
                // to $0 (silently pricing the model free) and a negative rate would yield negative
                // cost. Skip the row so the shipped/estimated price stands instead of being poisoned.
                let Some(input) = crate::config::dollars_to_micros(o.input_usd_per_mtok) else {
                    eprintln!(
                        "tare: ignoring invalid rate override for `{}` (rates must fit micro-USD)",
                        o.model
                    );
                    return None;
                };
                let Some(output) = crate::config::dollars_to_micros(o.output_usd_per_mtok) else {
                    eprintln!(
                        "tare: ignoring invalid rate override for `{}` (rates must fit micro-USD)",
                        o.model
                    );
                    return None;
                };
                let explicit_cache = match o.cache_read_usd_per_mtok {
                    Some(value) => match crate::config::dollars_to_micros(value) {
                        Some(value) => Some(value),
                        None => {
                            eprintln!(
                                "tare: ignoring invalid rate override for `{}` (rates must fit micro-USD)",
                                o.model
                            );
                            return None;
                        }
                    },
                    None => None,
                };
                // A shipped row for this model id (any provider) whose cache economics we can inherit.
                let base = self.models.iter().find(|r| r.model_id == o.model);
                let cache_read = match explicit_cache {
                    // An explicit override value is honored as-is (the user's call).
                    Some(c) => c,
                    // Inherited/defaulted: never let a cached READ cost MORE than fresh input — if the
                    // user overrode input BELOW the base's cache_read, that would be economically
                    // inverted. Clamp to input.
                    None => base
                        .map(|b| b.cache_read_micro_per_mtok)
                        .unwrap_or(input)
                        .min(input),
                };
                let cache_write_5m = base.map(|b| b.cache_write_5m_micro_per_mtok).unwrap_or(input);
                let cache_write_1h = base.map(|b| b.cache_write_1h_micro_per_mtok).unwrap_or(input);
                Some((
                    o.model.clone(),
                    ModelRates {
                        provider: String::new(), // provider-agnostic: keyed by raw model id
                        model_id: o.model.clone(),
                        tier: default_tier(),
                        input_micro_per_mtok: input,
                        output_micro_per_mtok: output,
                        cache_read_micro_per_mtok: cache_read,
                        cache_write_5m_micro_per_mtok: cache_write_5m,
                        cache_write_1h_micro_per_mtok: cache_write_1h,
                        audio_input_micro_per_mtok: 0,
                        audio_output_micro_per_mtok: 0,
                        context_tiers: Vec::new(),
                        effective_date: None,
                    },
                ))
            })
            .collect();
        self.model_overrides = built;
    }
}

fn validate_nonempty_pricing_field(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

fn validate_pricing_date(field: &str, value: &str) -> Result<(), String> {
    if crate::calendar::parse_date(value).is_none() {
        return Err(format!("{field} must be a valid YYYY-MM-DD date"));
    }
    Ok(())
}

impl PricingTable {
    /// Find a rates row by model id alone (any provider), for what-if swaps where the user
    /// names a target model without a provider. Returns the first match in table order.
    pub fn find_by_model(&self, model_id: &str) -> Option<&ModelRates> {
        self.models.iter().find(|m| m.model_id == model_id)
    }
}

/// Canonicalize a Bedrock/Vertex-decorated model id to the bare model id, or `None` when there is
/// nothing to strip. Handles an optional region prefix (`us.`, `eu.`, `apac.`, …), a vendor prefix
/// (`anthropic.`, `amazon.`, `meta.`, …), and a trailing version tag (`-v1:0`, `:0`), e.g.
/// `anthropic.claude-haiku-4-5-20251001-v1:0` → `claude-haiku-4-5-20251001` (which then prices
/// directly, or via the date-suffix strip → `claude-haiku-4-5`). Only strips KNOWN prefixes so a
/// dotted model name like `gpt-4.1` is never mangled (a decorated Bedrock id was
/// silently dropped to $0). Pure.
fn strip_bedrock_decoration(model_id: &str) -> Option<&str> {
    let mut s = model_id;
    let mut region_stripped = false;
    // Leading region prefix, then vendor prefix (Bedrock ids are `[region.]vendor.model`).
    for pre in ["us.", "eu.", "apac.", "ap.", "ca.", "sa.", "global."] {
        if let Some(r) = s.strip_prefix(pre) {
            s = r;
            region_stripped = true;
            break;
        }
    }
    let mut vendor_stripped = false;
    for pre in [
        "anthropic.",
        "amazon.",
        "meta.",
        "mistral.",
        "cohere.",
        "ai21.",
        "deepseek.",
        "stability.",
        "writer.",
        "luma.",
        "qwen.",
        "twelvelabs.",
    ] {
        if let Some(r) = s.strip_prefix(pre) {
            s = r;
            vendor_stripped = true;
            break;
        }
    }
    // A region-like prefix or a colon alone is not proof of Bedrock decoration. Requiring a known
    // vendor prevents ordinary ids such as `us.custom-model` or `model:variant` from being
    // redirected to an unrelated pricing row.
    if !vendor_stripped {
        return None;
    }
    // Trailing Bedrock version tag: `...-v1:0` → cut at ':' then drop a trailing `-vN`.
    if let Some((base, version)) = s.rsplit_once(':') {
        if !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()) {
            s = base;
        }
    }
    if let Some(dash) = s.rfind("-v") {
        let tail = &s[dash + 2..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            s = &s[..dash];
        }
    }
    if (region_stripped || vendor_stripped) && !s.is_empty() && s != model_id {
        Some(s)
    } else {
        None
    }
}

/// If `model_id` carries a dated-snapshot suffix, return the base id to retry in pricing lookup:
/// `claude-haiku-4-5-20251001` becomes `claude-haiku-4-5`, and `gpt-4o@2025-10-01` becomes
/// `gpt-4o`. Only a trailing valid `YYYYMMDD` after `-`, or a valid date after `@`, counts as a
/// snapshot (so `-4-5`, `-99999999`, and `@latest` do not).
fn strip_date_suffix(model_id: &str) -> Option<&str> {
    if let Some(i) = model_id.rfind('-') {
        let suffix = &model_id[i + 1..];
        if valid_compact_date(suffix) {
            return Some(&model_id[..i]);
        }
    }
    match model_id.find('@') {
        Some(i) if i > 0 && valid_snapshot_date(&model_id[i + 1..]) => Some(&model_id[..i]),
        _ => None,
    }
}

fn valid_compact_date(value: &str) -> bool {
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let canonical = format!("{}-{}-{}", &value[..4], &value[4..6], &value[6..]);
    crate::calendar::parse_date(&canonical).is_some()
}

fn valid_snapshot_date(value: &str) -> bool {
    crate::calendar::parse_date(value).is_some() || valid_compact_date(value)
}

/// One-time-per-model stderr diagnostic for an unpriced model. Dedup is process-global
/// so hot loops don't spam; it never touches stdout/artifacts, so determinism is unaffected.
fn warn_missing_pricing(provider: Provider, vendor: Option<&str>, model_id: &str) {
    static SEEN: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    // Key on the resolved pricing dimension so a missing groq/llama-3 warns distinctly from openai.
    let key = format!("{}/{}", provider.pricing_key(vendor), model_id);
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap();
    if seen.insert(key.clone()) {
        eprintln!("tare: no pricing row for {key}; treating as zero-cost (estimate incomplete)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modelsdev_json_converts_per_mtok_and_merge_prefers_litellm() {
        // models.dev shape: provider registry → models → cost (per-Mtok). Namespaced id → provider.
        let md = PricingTable::from_modelsdev_json(
            r#"{"requesty":{"models":{
                "anthropic/claude-opus-4-8":{"cost":{"input":5,"output":25,"cache_read":0.5,"cache_write":6.25}},
                "openai/gpt-z":{"cost":{"input":0.05,"output":0.4}}
            }}}"#,
            "2026-07-02",
        )
        .unwrap();
        let opus = md.find_by_model("claude-opus-4-8").unwrap();
        assert_eq!(opus.provider, "anthropic", "namespace becomes the provider");
        assert_eq!(opus.input_micro_per_mtok, 5_000_000); // 5 $/Mtok × 1e6
        assert_eq!(opus.cache_read_micro_per_mtok, 500_000);
        assert_eq!(opus.cache_write_5m_micro_per_mtok, 6_250_000);
        assert_eq!(opus.cache_write_1h_micro_per_mtok, 10_000_000); // 1.6× derived

        // LiteLLM (primary) has opus at a DIFFERENT rate + no gpt-z; models.dev has gpt-z.
        let litellm = PricingTable::from_litellm_json(
            r#"{"claude-opus-4-8":{"litellm_provider":"anthropic","input_cost_per_token":0.000006,"output_cost_per_token":0.000025}}"#,
            "2026-07-02",
        )
        .unwrap();
        let merged = litellm.merge_prefer(&md).unwrap();
        // Primary wins on the conflict (6/Mtok, not models.dev's 5).
        assert_eq!(
            merged
                .find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            6_000_000
        );
        // Secondary fills the gap (gpt-z only in models.dev).
        assert_eq!(
            merged.find_by_model("gpt-z").unwrap().input_micro_per_mtok,
            50_000
        );
    }

    #[test]
    fn pricing_tables_reject_invalid_or_ambiguous_rows() {
        let row = serde_json::json!({
            "provider": "anthropic",
            "model_id": "m",
            "input_micro_per_mtok": 1,
            "output_micro_per_mtok": 2,
            "cache_read_micro_per_mtok": 0,
            "cache_write_5m_micro_per_mtok": 0,
            "cache_write_1h_micro_per_mtok": 0
        });
        let table = |models: Vec<serde_json::Value>| {
            serde_json::json!({
                "version": "test",
                "effective_date": "2026-06-01",
                "models": models
            })
            .to_string()
        };

        assert!(PricingTable::from_json_str(&table(vec![row.clone()])).is_ok());

        let mut negative = row.clone();
        negative["input_micro_per_mtok"] = serde_json::json!(-1);
        assert!(PricingTable::from_json_str(&table(vec![negative]))
            .unwrap_err()
            .contains("must be non-negative"));

        let mut unknown = row.clone();
        unknown["input_price"] = serde_json::json!(1);
        assert!(PricingTable::from_json_str(&table(vec![unknown]))
            .unwrap_err()
            .contains("unknown field"));

        let duplicate = table(vec![row.clone(), row.clone()]);
        assert!(PricingTable::from_json_str(&duplicate)
            .unwrap_err()
            .contains("duplicate pricing edition"));

        let invalid_date = table(vec![row]).replace("2026-06-01", "2026-02-30");
        assert!(PricingTable::from_json_str(&invalid_date)
            .unwrap_err()
            .contains("valid YYYY-MM-DD"));
    }

    #[test]
    fn imported_rates_reject_malformed_nonfinite_negative_and_oversized_values() {
        for rate in ["\"nope\"", "\"NaN\"", "-0.01", "\"1e100\""] {
            let src = format!(
                r#"{{"m":{{"litellm_provider":"anthropic","input_cost_per_token":{rate}}}}}"#
            );
            assert!(
                PricingTable::from_litellm_json(&src, "2026-07-02").is_err(),
                "rate {rate} must be rejected"
            );
        }
        assert!(PricingTable::from_modelsdev_json(
            r#"{"p":{"models":{"m":{"cost":{"input":"NaN"}}}}}"#,
            "2026-07-02"
        )
        .is_err());
        assert!(PricingTable::from_litellm_json("{}", "2026-02-30").is_err());
    }

    #[test]
    fn as_of_vs_latest_repricing_picks_the_right_edition() {
        // One model, two dated editions: $3/Mtok input from 2026-06-01, bumped to $4 on 2026-07-01.
        let t = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,"output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":0,"cache_write_5m_micro_per_mtok":0,"cache_write_1h_micro_per_mtok":0,"effective_date":"2026-06-01"},
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":4000000,"output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":0,"cache_write_5m_micro_per_mtok":0,"cache_write_1h_micro_per_mtok":0,"effective_date":"2026-07-01"}]}"#,
        )
        .unwrap();
        let rate = |mode, date| {
            t.lookup_mode(Provider::Anthropic, None, "m", date, mode)
                .unwrap()
                .input_micro_per_mtok
        };
        // As-of: an old event prices at the old edition; a new event at the new one.
        assert_eq!(rate(PricingMode::AsOf, "2026-06-15"), 3_000_000);
        assert_eq!(rate(PricingMode::AsOf, "2026-07-15"), 4_000_000);
        // Latest: everything prices at the newest edition regardless of the event date.
        assert_eq!(rate(PricingMode::Latest, "2026-06-15"), 4_000_000);
        assert_eq!(rate(PricingMode::Latest, "2026-07-15"), 4_000_000);
        // Default mode is as-of (the honest, contemporaneous choice).
        assert_eq!(PricingMode::default(), PricingMode::AsOf);
    }

    #[test]
    fn litellm_json_converts_per_token_to_micro_mtok_and_dates_the_edition() {
        // Two real-shaped rows + a meta row that must be skipped.
        let src = r#"{
          "sample_spec": {"litellm_provider": "anthropic", "notes": "not a model"},
          "claude-opus-4-8": {
            "litellm_provider": "anthropic",
            "input_cost_per_token": 0.000005,
            "output_cost_per_token": 0.000025,
            "cache_read_input_token_cost": 0.0000005,
            "cache_creation_input_token_cost": 0.00000625
          },
          "openai/gpt-x": {
            "litellm_provider": "openai",
            "input_cost_per_token": 0.000001,
            "output_cost_per_token": 0.000002
          }
        }"#;
        let t = PricingTable::from_litellm_json(src, "2026-07-02").unwrap();
        assert_eq!(t.effective_date, "2026-07-02");
        assert!(t.version.contains("litellm"));
        // sample_spec skipped → exactly two models.
        assert_eq!(t.models.len(), 2);
        let opus = t.find_by_model("claude-opus-4-8").unwrap();
        // 0.000005 $/tok × 1e12 = 5_000_000 µ$/Mtok (rounded, not truncated).
        assert_eq!(opus.input_micro_per_mtok, 5_000_000);
        assert_eq!(opus.output_micro_per_mtok, 25_000_000);
        assert_eq!(opus.cache_read_micro_per_mtok, 500_000);
        assert_eq!(opus.cache_write_5m_micro_per_mtok, 6_250_000);
        // 1h derived from the 5m tier at the Anthropic 1.6× ratio (2.0/1.25).
        assert_eq!(opus.cache_write_1h_micro_per_mtok, 10_000_000);
        // The `provider/` prefix is stripped to the bare model id.
        assert!(t.find_by_model("gpt-x").is_some());
        // A provider with no cache fields → zero cache rates (never fabricated).
        let gpt = t.find_by_model("gpt-x").unwrap();
        assert_eq!(gpt.cache_write_5m_micro_per_mtok, 0);
        assert_eq!(gpt.cache_write_1h_micro_per_mtok, 0);
    }

    #[test]
    fn litellm_json_parses_the_above_200k_context_tier() {
        // a 1M-context model with above-200k rates → a ContextTier that `for_input`
        // applies for large prompts, exact from stored counts.
        let src = r#"{
          "claude-sonnet-1m": {
            "litellm_provider": "anthropic",
            "input_cost_per_token": 0.000003,
            "output_cost_per_token": 0.000015,
            "cache_read_input_token_cost": 0.0000003,
            "cache_creation_input_token_cost": 0.00000375,
            "input_cost_per_token_above_200k_tokens": 0.000006,
            "output_cost_per_token_above_200k_tokens": 0.0000225,
            "cache_read_input_token_cost_above_200k_tokens": 0.0000006
          }
        }"#;
        let t = PricingTable::from_litellm_json(src, "2026-07-05").unwrap();
        let m = t.find_by_model("claude-sonnet-1m").unwrap();
        assert_eq!(m.context_tiers.len(), 1);
        // Below the threshold: base rate. Above 200k: the tier rate.
        assert_eq!(m.for_input(100_000).input_micro_per_mtok, 3_000_000);
        let hi = m.for_input(250_000);
        assert_eq!(hi.input_micro_per_mtok, 6_000_000);
        assert_eq!(hi.output_micro_per_mtok, 22_500_000);
        assert_eq!(hi.cache_read_micro_per_mtok, 600_000);
        // A model with no above-200k fields carries no tier (never fabricated).
        let plain = PricingTable::from_litellm_json(
            r#"{"m":{"litellm_provider":"anthropic","input_cost_per_token":0.000003}}"#,
            "2026-07-05",
        )
        .unwrap();
        assert!(plain.find_by_model("m").unwrap().context_tiers.is_empty());
    }

    #[test]
    fn merged_with_prefers_self_and_backfills_from_fallback() {
        // LiteLLM (primary) wins where both define a model; models.dev backfills the rest.
        let primary = PricingTable::from_litellm_json(
            r#"{"claude-opus-4-8":{"litellm_provider":"anthropic","input_cost_per_token":0.000005}}"#,
            "2026-07-05",
        )
        .unwrap();
        let fallback = PricingTable::from_litellm_json(
            r#"{
              "claude-opus-4-8":{"litellm_provider":"anthropic","input_cost_per_token":0.000009},
              "gpt-5":{"litellm_provider":"openai","input_cost_per_token":0.000002}
            }"#,
            "2026-07-05",
        )
        .unwrap();
        let merged = primary.merged_with(&fallback);
        // opus stays at the primary rate ($5), not the fallback's $9.
        assert_eq!(
            merged
                .find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000
        );
        // gpt-5 (only in the fallback) is backfilled.
        assert_eq!(
            merged.find_by_model("gpt-5").unwrap().input_micro_per_mtok,
            2_000_000
        );
        // Version/date come from the primary.
        assert_eq!(merged.version, primary.version);
    }

    #[test]
    fn local_overlay_prices_per_backend_and_is_opt_in() {
        // A table with no local rows: local runs are unpriced (the status quo).
        let mut t = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[]}"#,
        )
        .unwrap();
        assert!(t
            .lookup(Provider::Local, Some("ollama"), "llama3")
            .is_none());

        // Overlay ollama at $0.50/Mtok (direct) and vllm via an energy estimate (2 kWh/Mtok × $0.15).
        use crate::config::LocalOverlay;
        t.set_local_overlay(&[
            LocalOverlay {
                backend: "ollama".into(),
                usd_per_mtok: Some(0.5),
                ..Default::default()
            },
            LocalOverlay {
                backend: "vllm".into(),
                kwh_per_mtok: Some(2.0),
                usd_per_kwh: Some(0.15),
                ..Default::default()
            },
            LocalOverlay {
                backend: "tgi".into(),
                ..Default::default()
            }, // no rate → skipped
        ]);
        // Any model on an overlaid backend gets the rate (input == output), applied per Mtok.
        let ollama = t.lookup(Provider::Local, Some("ollama"), "llama3").unwrap();
        assert_eq!(ollama.input_micro_per_mtok, 500_000);
        assert_eq!(ollama.output_micro_per_mtok, 500_000);
        let vllm = t.lookup(Provider::Local, Some("vllm"), "mixtral").unwrap();
        assert_eq!(vllm.input_micro_per_mtok, 300_000); // 2 * 0.15 = $0.30/Mtok
                                                        // A backend with no rate, and an unknown backend, stay unpriced.
        assert!(t.lookup(Provider::Local, Some("tgi"), "x").is_none());
        assert!(t.lookup(Provider::Local, Some("lmstudio"), "x").is_none());
        // A generic local run with no backend label is still unpriced (no vendor to key on).
        assert!(t.lookup(Provider::Local, None, "x").is_none());
    }

    #[test]
    fn context_tier_applies_long_context_rate_above_threshold() {
        // A model that charges 2x input above 200k tokens.
        let toml = r#"
version = "tiers"
effective_date = "2026-06-01"
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
input_micro_per_mtok = 5000000
output_micro_per_mtok = 25000000
cache_read_micro_per_mtok = 500000
cache_write_5m_micro_per_mtok = 6250000
cache_write_1h_micro_per_mtok = 10000000
[[model.context_tiers]]
over_tokens = 200000
input_micro_per_mtok = 10000000
output_micro_per_mtok = 50000000
"#;
        let table = PricingTable::from_toml_str(toml).unwrap();
        let rates = table
            .lookup(crate::model::Provider::Anthropic, None, "claude-opus-4-8")
            .unwrap();
        // Below the threshold: base input rate.
        assert_eq!(rates.for_input(100_000).input_micro_per_mtok, 5_000_000);
        // Exact boundary (`over_tokens` is INCLUSIVE, `>=`): one token below stays base, AT the
        // threshold flips to the tier. Locks the off-by-one on a money-critical edge.
        assert_eq!(rates.for_input(199_999).input_micro_per_mtok, 5_000_000);
        assert_eq!(rates.for_input(200_000).input_micro_per_mtok, 10_000_000);
        // Above: long-context rate; an un-overridden field (cache_read) keeps the base value.
        let hi = rates.for_input(250_000);
        assert_eq!(hi.input_micro_per_mtok, 10_000_000);
        assert_eq!(hi.output_micro_per_mtok, 50_000_000);
        assert_eq!(hi.cache_read_micro_per_mtok, 500_000);

        // End to end through the cost path: 300k fresh input bills at the long-context input rate.
        let usage = crate::model::UsageTokens {
            fresh_input: 300_000,
            ..Default::default()
        };
        let cost = crate::account::cost_from_usage(&usage, rates);
        assert_eq!(cost.fresh.micros(), 3_000_000); // 300k * $10/Mtok
    }

    #[test]
    fn lookup_on_selects_the_rate_in_effect_on_a_date() {
        // Two dated editions of the same model: a price hike on 2026-06-01.
        let toml = r#"
version = "dated"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
input_micro_per_mtok = 5000000
output_micro_per_mtok = 25000000
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
effective_date = "2026-06-01"
input_micro_per_mtok = 7000000
output_micro_per_mtok = 35000000
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#;
        let t = PricingTable::from_toml_str(toml).unwrap();
        let p = crate::model::Provider::Anthropic;
        // A May run is priced at the old rate; a June run at the new one.
        assert_eq!(
            t.lookup_on(p, None, "claude-opus-4-8", "2026-05-15")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000
        );
        assert_eq!(
            t.lookup_on(p, None, "claude-opus-4-8", "2026-06-15")
                .unwrap()
                .input_micro_per_mtok,
            7_000_000
        );
        // A date before all editions falls back to the earliest.
        assert_eq!(
            t.lookup_on(p, None, "claude-opus-4-8", "2025-01-01")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000
        );
    }

    #[test]
    fn as_of_collapses_to_a_single_edition_view_at_a_date() {
        // Same two dated editions ($5→$7 on 2026-06-01).
        let toml = r#"
version = "dated"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
input_micro_per_mtok = 5000000
output_micro_per_mtok = 25000000
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
effective_date = "2026-06-01"
input_micro_per_mtok = 7000000
output_micro_per_mtok = 35000000
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#;
        let t = PricingTable::from_toml_str(toml).unwrap();
        // A May snapshot is a single-edition view at the old rate; June at the new rate.
        let may = t.as_of("2026-05-15");
        assert_eq!(may.models.len(), 1, "one row per model in a snapshot view");
        assert_eq!(may.effective_date, "2026-05-15");
        assert_eq!(
            may.find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000
        );
        assert_eq!(
            t.as_of("2026-06-15")
                .find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            7_000_000
        );
        // Before all editions → earliest (mirrors lookup_on).
        assert_eq!(
            t.as_of("2025-01-01")
                .find_by_model("claude-opus-4-8")
                .unwrap()
                .input_micro_per_mtok,
            5_000_000
        );
    }

    #[test]
    fn lookup_as_of_date_selects_but_still_honors_overrides() {
        // "m": $3 (base) → $6 (2026-07-01).
        let toml = r#"
version = "d"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "m"
effective_date = "2026-07-01"
input_micro_per_mtok = 6000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#;
        let mut t = PricingTable::from_toml_str(toml).unwrap();
        let p = Provider::Anthropic;
        // Date-select the edition in effect on the date.
        assert_eq!(
            t.lookup_as_of(p, None, "m", Some("2026-06-15"))
                .unwrap()
                .input_micro_per_mtok,
            3_000_000
        );
        assert_eq!(
            t.lookup_as_of(p, None, "m", Some("2026-07-15"))
                .unwrap()
                .input_micro_per_mtok,
            6_000_000
        );
        // The `None` path is exactly the plain first-row lookup (byte-identical hot-path behavior).
        assert_eq!(
            t.lookup_as_of(p, None, "m", None)
                .unwrap()
                .input_micro_per_mtok,
            t.lookup(p, None, "m").unwrap().input_micro_per_mtok
        );
        // LANDMINE GUARD: a per-model override wins even WITH a date — date-aware
        // pricing must not silently drop overrides the way a naive lookup_on would.
        t.set_model_overrides(&[crate::config::ModelOverride {
            model: "m".into(),
            input_usd_per_mtok: 1.0, // $1/Mtok → 1_000_000 µ$
            output_usd_per_mtok: 0.0,
            cache_read_usd_per_mtok: None,
        }]);
        assert_eq!(
            t.lookup_as_of(p, None, "m", Some("2026-07-15"))
                .unwrap()
                .input_micro_per_mtok,
            1_000_000,
            "override must win over the dated table row"
        );
    }

    #[test]
    fn for_mode_latest_collapses_to_newest_asof_keeps_editions() {
        let toml = r#"
version = "d"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "m"
effective_date = "2026-07-01"
input_micro_per_mtok = 6000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#;
        let t = PricingTable::from_toml_str(toml).unwrap();
        assert_eq!(t.newest_edition_date(), "2026-07-01");
        // edition_dates: distinct dated editions ascending, incl. the table date.
        assert_eq!(t.edition_dates(), vec!["2026-01-01", "2026-07-01"]);
        // Latest → one row at the newest ($6).
        let latest = t.for_mode(PricingMode::Latest);
        assert_eq!(latest.models.len(), 1);
        assert_eq!(
            latest.find_by_model("m").unwrap().input_micro_per_mtok,
            6_000_000
        );
        // AsOf (default) → keeps both editions; plain lookup = first row ($3), byte-identical behavior.
        let asof = t.for_mode(PricingMode::AsOf);
        assert_eq!(asof.models.len(), 2);
        assert_eq!(
            asof.lookup(Provider::Anthropic, None, "m")
                .unwrap()
                .input_micro_per_mtok,
            3_000_000
        );
    }

    #[test]
    fn pricing_settings_deserializes_reprice_mode() {
        // The [pricing] reprice key drives the mode; absent → AsOf (byte-identical default).
        let latest: crate::config::PricingSettings =
            toml::from_str("reprice = \"latest\"").unwrap();
        assert_eq!(latest.reprice, PricingMode::Latest);
        let default: crate::config::PricingSettings = toml::from_str("").unwrap();
        assert_eq!(default.reprice, PricingMode::AsOf);
    }

    #[test]
    fn local_models_priced_via_a_local_provider_entry() {
        // The cloud-equivalent or custom-rate overlay for self-hosted models: a pricing file
        // with provider="local" entries prices Provider::Local usage; unlisted local models stay
        // unpriced (usage-first).
        let toml = r#"
version = "local-overlay"
effective_date = "2026-06-01"
[[model]]
provider = "local"
model_id = "gemma-2-9b"
input_micro_per_mtok = 200000
output_micro_per_mtok = 600000
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#;
        let t = PricingTable::from_toml_str(toml).unwrap();
        let r = t
            .lookup(Provider::Local, None, "gemma-2-9b")
            .expect("local model is priced");
        assert_eq!(r.input_micro_per_mtok, 200_000);
        assert_eq!(r.output_micro_per_mtok, 600_000);
        assert!(t.lookup(Provider::Local, None, "unlisted").is_none());
    }

    #[test]
    fn loads_fixture_toml() {
        let s = include_str!("../../pricing/pricing.fixture.toml");
        let t = PricingTable::from_toml_str(s).unwrap();
        assert_eq!(t.version, "fixture-2026.06");
        let opus = t
            .lookup(Provider::Anthropic, None, "claude-opus-4-8")
            .unwrap();
        assert_eq!(opus.input_micro_per_mtok, 5_000_000);
        assert_eq!(opus.cache_read_micro_per_mtok, 500_000);
        assert_eq!(opus.cache_write_5m_micro_per_mtok, 6_250_000);
        assert_eq!(opus.cache_write_1h_micro_per_mtok, 10_000_000);
    }

    #[test]
    fn openai_compatible_prices_per_vendor(/* */) {
        let t = PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml"))
            .unwrap();
        let model = "llama-3.1-70b";
        // The same model name resolves to different rates per vendor label…
        let groq = t
            .lookup(Provider::OpenAiCompatible, Some("groq"), model)
            .unwrap();
        let together = t
            .lookup(Provider::OpenAiCompatible, Some("together"), model)
            .unwrap();
        assert_eq!(groq.input_micro_per_mtok, 590_000);
        assert_eq!(together.input_micro_per_mtok, 880_000);
        // …an unknown vendor (or none) finds no row -> unpriced (usage-first, never a wrong $).
        assert!(t
            .lookup(Provider::OpenAiCompatible, Some("unlisted"), model)
            .is_none());
        assert!(t.lookup(Provider::OpenAiCompatible, None, model).is_none());
        // pricing_key composes the (provider, model) key from the vendor.
        assert_eq!(
            Provider::OpenAiCompatible
                .pricing_key(Some("groq"))
                .as_ref(),
            "groq"
        );
        assert_eq!(
            Provider::OpenAiCompatible.pricing_key(None).as_ref(),
            "openai_compatible"
        );
        assert_eq!(
            Provider::Openai.pricing_key(Some("groq")).as_ref(),
            "openai"
        );
    }

    #[test]
    fn loads_shipped_json() {
        let s = include_str!("../../pricing/pricing.json");
        let t = PricingTable::from_json_str(s).unwrap();
        assert_eq!(t.version, "2026.06.01");
        assert!(t.lookup(Provider::Openai, None, "gpt-5-mini").is_some());
        // gpt-4o* rows are quarantined because they contradict this fixture's gpt-5-only scope.
        assert!(t.lookup(Provider::Openai, None, "gpt-4o-mini").is_none());
        assert!(t.lookup(Provider::Openai, None, "gpt-4o").is_none());
    }

    #[test]
    fn lookup_index_matches_linear_scan() {
        let t = PricingTable::from_json_str(include_str!("../../pricing/pricing.json")).unwrap();
        for m in &t.models {
            // A row whose provider tag isn't a Provider enum value is an OpenAI-compatible vendor
            // row: look it up as OpenAiCompatible + that vendor label.
            let (p, vendor) = match Provider::parse(&m.provider) {
                Some(p) => (p, None),
                None => (Provider::OpenAiCompatible, Some(m.provider.as_str())),
            };
            let viaidx = t.lookup(p, vendor, &m.model_id).unwrap();
            let viascan = t
                .models
                .iter()
                .find(|r| r.provider == m.provider && r.model_id == m.model_id)
                .unwrap();
            assert_eq!(viaidx, viascan);
        }
    }

    #[test]
    fn strip_date_suffix_only_strips_real_date_shapes() {
        assert_eq!(
            strip_date_suffix("claude-haiku-4-5-20251001"),
            Some("claude-haiku-4-5")
        );
        assert_eq!(strip_date_suffix("gpt-4o@2025-10-01"), Some("gpt-4o"));
        assert_eq!(strip_date_suffix("claude-haiku-4-5"), None); // "-5" isn't 8 digits
        assert_eq!(strip_date_suffix("gpt-4o"), None);
        assert_eq!(strip_date_suffix("model-1234567"), None); // 7 digits, not a date
        assert_eq!(strip_date_suffix("model-20250229"), None); // impossible calendar date
        assert_eq!(strip_date_suffix("model-99999999"), None);
        assert_eq!(strip_date_suffix("model@latest"), None);
        assert_eq!(strip_date_suffix("model@20251001"), Some("model"));
        assert_eq!(strip_date_suffix("@2025"), None); // leading @ → empty base, rejected
    }

    fn haiku_base_toml() -> &'static str {
        r#"
version = "t"
effective_date = "2026-06-01"
[[model]]
provider = "anthropic"
model_id = "claude-haiku-4-5"
input_micro_per_mtok = 1000000
output_micro_per_mtok = 5000000
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#
    }

    #[test]
    fn dated_snapshot_id_falls_back_to_base_pricing() {
        // Only the base row exists; a dated-snapshot alias prices via the base, not $0.
        let t = PricingTable::from_toml_str(haiku_base_toml()).unwrap();
        let p = crate::model::Provider::Anthropic;
        let r = t
            .lookup(p, None, "claude-haiku-4-5-20251001")
            .expect("dated alias resolves to the base row");
        assert_eq!(r.input_micro_per_mtok, 1_000_000);
        assert!(t.lookup(p, None, "claude-haiku-4-5@2025-10-01").is_some()); // @date form too
        assert!(t.lookup(p, None, "claude-haiku-4-5").is_some()); // base still direct
                                                                  // No base present → still unpriced (not mis-stripped into a wrong row); unknown stays None.
        assert!(t.lookup(p, None, "claude-opus-9-9-20251001").is_none());
        assert!(t.lookup(p, None, "totally-unknown").is_none());
    }

    #[test]
    fn bedrock_decorated_id_prices_via_the_bare_row() {
        // `anthropic.claude-haiku-4-5-20251001-v1:0` was once dropped to $0. It must price
        // via the base row (decoration strip → dated alias → base), not degrade to unpriced.
        let t = PricingTable::from_toml_str(haiku_base_toml()).unwrap();
        let p = crate::model::Provider::Anthropic;
        let r = t
            .lookup(p, None, "anthropic.claude-haiku-4-5-20251001-v1:0")
            .expect("decorated Bedrock id resolves to the base row");
        assert_eq!(r.input_micro_per_mtok, 1_000_000);
        // region-prefixed inference-profile form too.
        assert!(t
            .lookup(p, None, "us.anthropic.claude-haiku-4-5-20251001-v1:0")
            .is_some());
        assert!(t
            .lookup(p, None, "anthropic.claude-haiku-4-5-v1:0")
            .is_some());
        // A dotted non-Bedrock model name must NOT be mangled by the prefix strip.
        assert_eq!(strip_bedrock_decoration("gpt-4.1"), None);
        assert_eq!(strip_bedrock_decoration("claude-haiku-4-5"), None);
        assert_eq!(strip_bedrock_decoration("model:0"), None);
        assert_eq!(strip_bedrock_decoration("us.custom-model"), None);
        assert_eq!(
            strip_bedrock_decoration("anthropic.claude-haiku-4-5-20251001-v1:0"),
            Some("claude-haiku-4-5-20251001")
        );
        // Truly-unknown decorated model stays unpriced (not mis-stripped into a wrong row).
        assert!(t
            .lookup(p, None, "anthropic.claude-opus-9-9-v1:0")
            .is_none());
    }

    #[test]
    fn user_override_prices_an_unpriced_model_and_wins_over_the_table() {
        let mut t = PricingTable::from_toml_str(haiku_base_toml()).unwrap();
        let p = crate::model::Provider::Anthropic;
        assert!(t.lookup(p, None, "my-local-llama").is_none()); // unpriced before overrides
        t.set_model_overrides(&[
            crate::config::ModelOverride {
                model: "my-local-llama".into(),
                input_usd_per_mtok: 0.5,
                output_usd_per_mtok: 1.5,
                cache_read_usd_per_mtok: None,
            },
            crate::config::ModelOverride {
                model: "claude-haiku-4-5".into(), // overrides an existing base row
                input_usd_per_mtok: 9.0,
                output_usd_per_mtok: 9.0,
                cache_read_usd_per_mtok: Some(0.9),
            },
        ]);
        // Previously-unpriced model now prices via the override (0.5 USD/Mtok → 500_000 µ$/Mtok).
        let r = t
            .lookup(p, None, "my-local-llama")
            .expect("override prices it");
        assert_eq!(r.input_micro_per_mtok, 500_000);
        assert_eq!(r.output_micro_per_mtok, 1_500_000);
        assert_eq!(r.cache_read_micro_per_mtok, 500_000); // defaults to input rate
                                                          // Override WINS over the shipped base row.
        let h = t.lookup(p, None, "claude-haiku-4-5").unwrap();
        assert_eq!(h.input_micro_per_mtok, 9_000_000);
        assert_eq!(h.cache_read_micro_per_mtok, 900_000);
    }

    #[test]
    fn with_config_applies_overrides_not_just_overlay() {
        // the ONE shared assembly (called by both load_pricing and the desktop
        // pricing()) must apply per-model overrides, not only the self-hosted overlay — the desktop
        // bug dropped overrides, diverging every desktop dollar from the CLI.
        let base = PricingTable::from_toml_str(haiku_base_toml()).unwrap();
        let mut cfg = crate::config::TareConfig::default();
        cfg.pricing.overrides = vec![crate::config::ModelOverride {
            model: "claude-haiku-4-5".into(),
            input_usd_per_mtok: 9.0,
            output_usd_per_mtok: 9.0,
            cache_read_usd_per_mtok: None,
        }];
        let t = base.with_config(&cfg);
        let r = t
            .lookup(crate::model::Provider::Anthropic, None, "claude-haiku-4-5")
            .expect("row exists");
        assert_eq!(
            r.input_micro_per_mtok, 9_000_000,
            "with_config must fold in [pricing] overrides"
        );
        // Overriding a priced row inherits its real cache economics (iolq #4), not 1x input.
        assert!(
            r.cache_read_micro_per_mtok < r.input_micro_per_mtok,
            "priced-row override keeps the sub-1x cache_read, not collapsed to input"
        );
    }

    #[test]
    fn no_overrides_leaves_pricing_byte_identical() {
        let mut t = PricingTable::from_toml_str(haiku_base_toml()).unwrap();
        let p = crate::model::Provider::Anthropic;
        let before = t
            .lookup(p, None, "claude-haiku-4-5")
            .unwrap()
            .input_micro_per_mtok;
        t.set_model_overrides(&[]);
        assert_eq!(
            t.lookup(p, None, "claude-haiku-4-5")
                .unwrap()
                .input_micro_per_mtok,
            before
        );
    }

    #[test]
    fn explicit_dated_row_wins_over_base_fallback() {
        // BOTH a base and a dated-alias row, different rates → the alias hits its OWN row, not the base.
        let toml = format!(
            "{}[[model]]\nprovider = \"anthropic\"\nmodel_id = \"claude-haiku-4-5-20251001\"\n\
             input_micro_per_mtok = 2000000\noutput_micro_per_mtok = 9000000\n\
             cache_read_micro_per_mtok = 0\ncache_write_5m_micro_per_mtok = 0\ncache_write_1h_micro_per_mtok = 0\n",
            haiku_base_toml()
        );
        let t = PricingTable::from_toml_str(&toml).unwrap();
        let p = crate::model::Provider::Anthropic;
        assert_eq!(
            t.lookup(p, None, "claude-haiku-4-5-20251001")
                .unwrap()
                .input_micro_per_mtok,
            2_000_000,
            "explicit dated row wins over the base fallback"
        );
    }
}
