//! Alert evaluation: turn a budget `Decision` transition into fire-once alerts. Pure and
//! deterministic (no clock/RNG/IO) — the daemon wires the side-effecting sinks around this.
//! Counts/causes only; alerts never carry payload.

use crate::budget::Decision;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Warn,
    Kill,
}

impl AlertLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertLevel::Warn => "warn",
            AlertLevel::Kill => "kill",
        }
    }
}

/// What an alert is ABOUT. A budget alert names a run; a daemon spend-anomaly alert names a
/// `(date, series_key, kind)` point — which does NOT fit a single `run_id`, hence the enum.
/// Internally tagged + flattened into `Alert`, so the JSON stays a flat object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "subject", rename_all = "lowercase")]
pub enum AlertSubject {
    Run {
        run_id: String,
    },
    Anomaly {
        date: String,
        series_key: String,
        kind: String,
    },
    /// A periodic monitor rule (built-in or configured). `key` is a stable, payload-free identity
    /// for the rule and observation, used by the daemon's persistent fire-once set.
    Monitor {
        date: String,
        metric: String,
        key: String,
    },
}

impl AlertSubject {
    pub fn run(run_id: impl Into<String>) -> Self {
        AlertSubject::Run {
            run_id: run_id.into(),
        }
    }
    /// Stable identity for fire-once dedup — for an anomaly this is `(date, series_key, kind)`.
    pub fn dedup_key(&self) -> String {
        match self {
            AlertSubject::Run { run_id } => format!("run:{run_id}"),
            AlertSubject::Anomaly {
                date,
                series_key,
                kind,
            } => format!("anomaly:{date}:{series_key}:{kind}"),
            AlertSubject::Monitor { date, metric, key } => {
                format!("monitor:{date}:{metric}:{key}")
            }
        }
    }
    /// Short human label for the stderr sink.
    pub fn label(&self) -> String {
        match self {
            AlertSubject::Run { run_id } => format!("run {run_id}"),
            AlertSubject::Anomaly {
                date,
                series_key,
                kind,
            } => format!("anomaly {kind} on {series_key} @ {date}"),
            AlertSubject::Monitor { date, metric, .. } => {
                format!("{metric} monitor @ {date}")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub level: AlertLevel,
    #[serde(flatten)]
    pub subject: AlertSubject,
    pub message: String,
}

/// Counts-only inputs for evaluating the periodic budget and user-configured monitor rules.
pub struct MonitorSnapshot<'a> {
    pub day: &'a str,
    pub today_micros: i64,
    pub budget: &'a crate::budget::PeriodBudget,
    pub run_rate_micros_per_day: Option<i64>,
    pub anomalies: &'a [crate::anomaly::Anomaly],
    pub anomalies_by_window: &'a BTreeMap<u32, Vec<crate::anomaly::Anomaly>>,
    pub captured_events: u64,
}

fn rule_key(rule: &crate::config::AlertRule) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        rule.metric,
        rule.threshold
            .map(|value| format!("{:016x}", value.to_bits()))
            .unwrap_or_default(),
        rule.kind.as_deref().unwrap_or_default(),
        rule.window_days
            .map(|value| value.to_string())
            .unwrap_or_default(),
        rule.min_events
            .map(|value| value.to_string())
            .unwrap_or_default(),
    )
}

fn monitor_alert(day: &str, metric: &str, key: String, message: String) -> Alert {
    Alert {
        level: AlertLevel::Warn,
        subject: AlertSubject::Monitor {
            date: day.to_string(),
            metric: metric.to_string(),
            key,
        },
        message,
    }
}

fn anomaly_kind_name(kind: crate::anomaly::AnomalyKind) -> &'static str {
    match kind {
        crate::anomaly::AnomalyKind::Spike => "spike",
        crate::anomaly::AnomalyKind::NewSeries => "new_series",
        crate::anomaly::AnomalyKind::VanishedSeries => "vanished_series",
    }
}

/// Evaluate the built-in periodic-budget notice and every configured `[[alert]]` rule. This is pure;
/// the caller owns data collection and atomically claims each returned subject before delivery.
pub fn evaluate_monitor_alerts(
    rules: &[crate::config::AlertRule],
    snapshot: &MonitorSnapshot<'_>,
) -> Vec<Alert> {
    use crate::money::MicroUsd;

    let mut alerts = Vec::new();
    if snapshot.budget.cap_micros > 0 && snapshot.budget.pct >= 100 {
        alerts.push(monitor_alert(
            snapshot.day,
            "period_pct",
            format!(
                "builtin:over:{}:{}",
                snapshot.budget.period, snapshot.budget.cap_micros
            ),
            format!(
                "{} budget reached {}% ({})",
                snapshot.budget.period,
                snapshot.budget.pct,
                MicroUsd(snapshot.budget.spent_micros).to_dollar_string()
            ),
        ));
    } else if snapshot.budget.cap_micros > 0 && snapshot.budget.pct >= snapshot.budget.warn_pct {
        alerts.push(monitor_alert(
            snapshot.day,
            "period_pct",
            format!(
                "builtin:warn:{}:{}",
                snapshot.budget.period, snapshot.budget.cap_micros
            ),
            format!(
                "{} budget reached {}% ({})",
                snapshot.budget.period,
                snapshot.budget.pct,
                MicroUsd(snapshot.budget.spent_micros).to_dollar_string()
            ),
        ));
    }

    for rule in rules {
        if rule
            .min_events
            .is_some_and(|minimum| snapshot.captured_events < u64::from(minimum))
        {
            continue;
        }
        let identity = rule_key(rule);
        match rule.metric.as_str() {
            "today_spend" => {
                let Some(threshold) = rule.threshold.and_then(crate::config::dollars_to_micros)
                else {
                    continue;
                };
                if snapshot.today_micros >= threshold {
                    alerts.push(monitor_alert(
                        snapshot.day,
                        "today_spend",
                        identity,
                        format!(
                            "today spend {} reached your {} alert",
                            MicroUsd(snapshot.today_micros).to_dollar_string(),
                            MicroUsd(threshold).to_dollar_string()
                        ),
                    ));
                }
            }
            "period_pct" => {
                let Some(threshold) = rule.threshold else {
                    continue;
                };
                if snapshot.budget.pct as f64 >= threshold {
                    alerts.push(monitor_alert(
                        snapshot.day,
                        "period_pct",
                        identity,
                        format!(
                            "{} budget reached {}% (custom {}% alert)",
                            snapshot.budget.period, snapshot.budget.pct, threshold
                        ),
                    ));
                }
            }
            "run_rate" => {
                let Some((threshold, rate)) = rule
                    .threshold
                    .and_then(crate::config::dollars_to_micros)
                    .zip(snapshot.run_rate_micros_per_day)
                else {
                    continue;
                };
                if rate >= threshold {
                    alerts.push(monitor_alert(
                        snapshot.day,
                        "run_rate",
                        identity,
                        format!(
                            "run rate {}/day reached your {}/day alert",
                            MicroUsd(rate).to_dollar_string(),
                            MicroUsd(threshold).to_dollar_string()
                        ),
                    ));
                }
            }
            "anomaly_kind" => {
                let source = match rule.window_days {
                    Some(window) => snapshot
                        .anomalies_by_window
                        .get(&window)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    None => snapshot.anomalies,
                };
                let wanted = rule.kind.as_deref().unwrap_or("any");
                for anomaly in source {
                    let kind = anomaly_kind_name(anomaly.kind);
                    if wanted != "any" && wanted != kind {
                        continue;
                    }
                    alerts.push(monitor_alert(
                        &anomaly.date,
                        "anomaly_kind",
                        format!("{identity}:{}", crate::anomaly::anomaly_key(anomaly)),
                        format!(
                            "configured anomaly: {} on {} ({kind})",
                            anomaly.series_key, anomaly.date
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    alerts
}

/// Severity rank so we can detect a strict escalation (fire-once-on-crossing).
fn severity(d: &Decision) -> u8 {
    match d {
        Decision::Allow => 0,
        Decision::Warn(_) => 1,
        Decision::Kill(_) => 2,
    }
}

/// Emit an alert iff `curr` is a STRICT escalation over `prev` (Allow→Warn, Allow→Kill,
/// Warn→Kill). Re-issuing the same level (Warn→Warn) fires nothing — alerts trigger once on
/// the crossing, so a runaway loop doesn't spam. `prev` is the last-seen decision for the run
/// (`Decision::Allow` before the first step).
pub fn evaluate(prev: &Decision, curr: &Decision, run_id: &str) -> Option<Alert> {
    if severity(curr) <= severity(prev) {
        return None;
    }
    match curr {
        Decision::Allow => None,
        Decision::Warn(msg) => Some(Alert {
            level: AlertLevel::Warn,
            subject: AlertSubject::run(run_id),
            message: msg.clone(),
        }),
        Decision::Kill(msg) => Some(Alert {
            level: AlertLevel::Kill,
            subject: AlertSubject::run(run_id),
            message: msg.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_on_strict_escalation() {
        let allow = Decision::Allow;
        let warn = Decision::Warn("80%".into());
        let kill = Decision::Kill("limit".into());
        // Allow -> Warn fires.
        assert_eq!(
            evaluate(&allow, &warn, "r").unwrap().level,
            AlertLevel::Warn
        );
        // Warn -> Warn does NOT re-fire.
        assert!(evaluate(&warn, &warn, "r").is_none());
        // Warn -> Kill fires.
        assert_eq!(evaluate(&warn, &kill, "r").unwrap().level, AlertLevel::Kill);
        // Kill -> Kill does not re-fire (kill-switch already tripped).
        assert!(evaluate(&kill, &kill, "r").is_none());
        // De-escalation never fires.
        assert!(evaluate(&kill, &warn, "r").is_none());
        // Allow -> Kill (jump) fires Kill.
        assert_eq!(
            evaluate(&allow, &kill, "r").unwrap().level,
            AlertLevel::Kill
        );
    }

    #[test]
    fn monitor_rules_cover_budget_spend_rate_anomaly_and_noise_floor() {
        use crate::anomaly::{Anomaly, AnomalyKind};
        use crate::budget::period_status;
        use crate::config::AlertRule;

        let anomaly = Anomaly {
            date: "2026-09-17".into(),
            series_key: "provider/model".into(),
            kind: AnomalyKind::Spike,
            value_micros: 2_000_000,
            baseline_micros: 500_000,
            materiality: "minor".into(),
        };
        let budget = period_status("month", 800_000, 1_000_000, 80);
        let rules = vec![
            AlertRule {
                metric: "today_spend".into(),
                threshold: Some(1.0),
                kind: None,
                window_days: None,
                min_events: None,
            },
            AlertRule {
                metric: "run_rate".into(),
                threshold: Some(2.0),
                kind: None,
                window_days: None,
                min_events: Some(10),
            },
            AlertRule {
                metric: "anomaly_kind".into(),
                threshold: None,
                kind: Some("spike".into()),
                window_days: None,
                min_events: None,
            },
        ];
        let snapshot = MonitorSnapshot {
            day: "2026-09-17",
            today_micros: 1_500_000,
            budget: &budget,
            run_rate_micros_per_day: Some(3_000_000),
            anomalies: std::slice::from_ref(&anomaly),
            anomalies_by_window: &BTreeMap::new(),
            captured_events: 9,
        };
        let alerts = evaluate_monitor_alerts(&rules, &snapshot);

        assert_eq!(alerts.len(), 3, "built-in budget + spend + anomaly");
        assert!(alerts
            .iter()
            .any(|alert| alert.message.contains("budget reached 80%")));
        assert!(alerts
            .iter()
            .any(|alert| alert.message.contains("today spend")));
        assert!(alerts
            .iter()
            .any(|alert| alert.message.contains("configured anomaly")));
        assert!(!alerts
            .iter()
            .any(|alert| alert.message.contains("run rate")));
        assert!(alerts
            .iter()
            .all(|alert| matches!(alert.subject, AlertSubject::Monitor { .. })));
    }

    #[test]
    fn monitor_uses_configured_budget_warning_and_stable_rule_identities() {
        use crate::budget::period_status;
        use crate::config::AlertRule;

        let below = period_status("month", 89, 100, 90);
        let empty_windows = BTreeMap::new();
        let snapshot = MonitorSnapshot {
            day: "2026-09-17",
            today_micros: 2_000_000,
            budget: &below,
            run_rate_micros_per_day: None,
            anomalies: &[],
            anomalies_by_window: &empty_windows,
            captured_events: 0,
        };
        assert!(evaluate_monitor_alerts(&[], &snapshot).is_empty());

        let at_warning = period_status("month", 90, 100, 90);
        let warning_snapshot = MonitorSnapshot {
            budget: &at_warning,
            ..snapshot
        };
        assert_eq!(evaluate_monitor_alerts(&[], &warning_snapshot).len(), 1);

        let rules = vec![
            AlertRule {
                metric: "today_spend".into(),
                threshold: Some(1.0),
                kind: None,
                window_days: None,
                min_events: None,
            },
            AlertRule {
                metric: "period_pct".into(),
                threshold: Some(50.0),
                kind: None,
                window_days: None,
                min_events: None,
            },
        ];
        let identities = |configured: &[AlertRule]| {
            evaluate_monitor_alerts(configured, &warning_snapshot)
                .into_iter()
                .filter(|alert| {
                    matches!(
                        &alert.subject,
                        AlertSubject::Monitor { key, .. } if !key.starts_with("builtin:")
                    )
                })
                .map(|alert| alert.subject.dedup_key())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let mut reordered = rules.clone();
        reordered.reverse();
        assert_eq!(identities(&rules), identities(&reordered));
    }

    #[test]
    fn anomaly_rule_uses_its_requested_window() {
        use crate::anomaly::{Anomaly, AnomalyKind};
        use crate::budget::period_status;
        use crate::config::AlertRule;

        let windowed = Anomaly {
            date: "2026-09-16".into(),
            series_key: "windowed".into(),
            kind: AnomalyKind::NewSeries,
            value_micros: 1,
            baseline_micros: 0,
            materiality: "minor".into(),
        };
        let mut windows = BTreeMap::new();
        windows.insert(30, vec![windowed]);
        let rule = AlertRule {
            metric: "anomaly_kind".into(),
            threshold: None,
            kind: Some("new_series".into()),
            window_days: Some(30),
            min_events: None,
        };
        let budget = period_status("month", 0, 0, 80);
        let snapshot = MonitorSnapshot {
            day: "2026-09-17",
            today_micros: 0,
            budget: &budget,
            run_rate_micros_per_day: None,
            anomalies: &[],
            anomalies_by_window: &windows,
            captured_events: 0,
        };
        let alerts = evaluate_monitor_alerts(&[rule], &snapshot);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].message.contains("windowed"));
    }
}
