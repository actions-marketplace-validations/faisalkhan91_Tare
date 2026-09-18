//! speedscope file-format export. Emits the documented top-level shape with two
//! evented profiles (weighted by tokens and by micro-dollars). Deterministic.
//! Validated structurally against the committed field list — no external tool.

use crate::flamegraph::{FlamegraphModel, FlamegraphNode};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const SCHEMA_URL: &str = "https://www.speedscope.app/file-format-schema.json";

/// Required top-level / profile field names we embed (mirror of schema/speedscope.fields.json).
pub const REQUIRED_TOP_FIELDS: &[&str] = &["$schema", "shared", "profiles", "exporter", "name"];
pub const REQUIRED_PROFILE_FIELDS: &[&str] =
    &["type", "name", "unit", "startValue", "endValue", "events"];

struct FrameTable {
    index: BTreeMap<String, usize>,
    frames: Vec<Value>,
}

impl FrameTable {
    fn new() -> Self {
        FrameTable {
            index: BTreeMap::new(),
            frames: Vec::new(),
        }
    }
    fn intern(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.frames.len();
        self.frames.push(json!({ "name": name }));
        self.index.insert(name.to_string(), i);
        i
    }
}

fn weight(node: &FlamegraphNode, by_dollars: bool) -> i64 {
    if by_dollars {
        node.micros
    } else {
        i64::try_from(node.tokens).unwrap_or(i64::MAX)
    }
}

fn emit(
    node: &FlamegraphNode,
    by_dollars: bool,
    cursor: &mut i64,
    frames: &mut FrameTable,
    events: &mut Vec<Value>,
) {
    let frame = frames.intern(&node.name);
    let start = *cursor;
    events.push(json!({"type": "O", "frame": frame, "at": start}));
    if node.children.is_empty() {
        *cursor = cursor.saturating_add(weight(node, by_dollars));
    } else {
        for child in &node.children {
            emit(child, by_dollars, cursor, frames, events);
        }
        // Containers may carry weight not covered by children; advance to keep close >= open.
        let target = start.saturating_add(weight(node, by_dollars));
        if *cursor < target {
            *cursor = target;
        }
    }
    events.push(json!({"type": "C", "frame": frame, "at": *cursor}));
}

fn profile(model: &FlamegraphModel, by_dollars: bool, frames: &mut FrameTable) -> Value {
    let mut cursor = 0i64;
    let mut events = Vec::new();
    emit(&model.root, by_dollars, &mut cursor, frames, &mut events);
    json!({
        "type": "evented",
        "name": if by_dollars { "micro-dollars" } else { "tokens" },
        "unit": "none",
        "startValue": 0,
        "endValue": cursor,
        "events": events,
    })
}

pub fn export(model: &FlamegraphModel, exporter_version: &str) -> Value {
    let mut frames = FrameTable::new();
    let token_profile = profile(model, false, &mut frames);
    let dollar_profile = profile(model, true, &mut frames);
    json!({
        "$schema": SCHEMA_URL,
        "shared": { "frames": frames.frames },
        "profiles": [token_profile, dollar_profile],
        "exporter": format!("tare@{exporter_version}"),
        "name": format!("tare run {}", model.run_id),
        "activeProfileIndex": 0,
    })
}

/// Structural self-validation against the embedded field list.
pub fn validate_structure(v: &Value) -> Result<(), String> {
    for f in REQUIRED_TOP_FIELDS {
        if v.get(*f).is_none() {
            return Err(format!("speedscope: missing top-level field `{f}`"));
        }
    }
    if v["$schema"] != json!(SCHEMA_URL) {
        return Err("speedscope: wrong $schema".into());
    }
    let frames = v["shared"]["frames"]
        .as_array()
        .ok_or("speedscope: shared.frames not an array")?;
    let profiles = v["profiles"]
        .as_array()
        .ok_or("speedscope: profiles not an array")?;
    if profiles.is_empty() {
        return Err("speedscope: no profiles".into());
    }
    for p in profiles {
        for f in REQUIRED_PROFILE_FIELDS {
            if p.get(*f).is_none() {
                return Err(format!("speedscope: profile missing `{f}`"));
            }
        }
        let events = p["events"]
            .as_array()
            .ok_or("speedscope: events not an array")?;
        for e in events {
            let frame = e["frame"]
                .as_u64()
                .ok_or("speedscope: event frame not an index")?;
            let frame =
                usize::try_from(frame).map_err(|_| "speedscope: event frame index out of range")?;
            if frame >= frames.len() {
                return Err("speedscope: event frame index out of range".into());
            }
            match e["type"].as_str() {
                Some("O") | Some("C") => {}
                _ => return Err("speedscope: event type must be O or C".into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_token_weights_saturate_the_profile_clock() {
        let model = FlamegraphModel {
            run_id: "large".into(),
            pricing_version: "v".into(),
            effective_date: "2026-01-01".into(),
            root: FlamegraphNode {
                name: "root".into(),
                tokens: u64::MAX,
                micros: i64::MAX,
                cache_class: None,
                children: vec![FlamegraphNode {
                    name: "leaf".into(),
                    tokens: u64::MAX,
                    micros: i64::MAX,
                    cache_class: None,
                    children: vec![],
                }],
            },
        };
        let exported = export(&model, "test");
        validate_structure(&exported).unwrap();
        assert_eq!(exported["profiles"][0]["endValue"], json!(i64::MAX));
        assert_eq!(exported["profiles"][1]["endValue"], json!(i64::MAX));
    }
}
