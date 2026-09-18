//! Minimal Prometheus text-exposition parsing for self-hosted inference token counters (the
//! homelab agent's non-invasive scrape path). Pure + unit-tested. Extracts cumulative token
//! counters per model from a vLLM / llama.cpp `/metrics` endpoint, and computes per-interval
//! deltas (handling counter resets on a server restart). The agent turns these deltas into OTLP
//! spans it ships to the hub. Unit coverage uses representative exposition text and does not launch
//! an inference server.

use std::collections::BTreeMap;

/// Cumulative token counters for one model, as scraped (monotonic until the server restarts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenCounters {
    pub prompt: u64,
    pub generation: u64,
}

/// Which token counter (if any) a metric name represents.
fn classify(name: &str) -> (bool, bool) {
    let is_prompt = matches!(
        name,
        "vllm:prompt_tokens_total" | "llamacpp:prompt_tokens_total"
    );
    let is_gen = matches!(
        name,
        "vllm:generation_tokens_total" | "llamacpp:tokens_predicted_total"
    );
    (is_prompt, is_gen)
}

/// Split `metric{label="v",...}` into (`metric`, model_name label) — or (`metric`, None) when
/// there are no labels.
fn split_name_labels(head: &str) -> (&str, Option<String>) {
    match head.split_once('{') {
        None => (head, None),
        Some((name, rest)) => {
            let labels = rest.trim_end_matches('}');
            let model = labels.split(',').find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                if k.trim() == "model_name" || k.trim() == "model" {
                    Some(v.trim().trim_matches('"').to_string())
                } else {
                    None
                }
            });
            (name, model)
        }
    }
}

/// Parse one Prometheus sample line `name{labels}? value [timestamp]?` into (metric name, model
/// label, value). The identifier (name + optional `{...}`) is taken STRUCTURALLY from the front (up to
/// the matching `}`), so a label value containing a space doesn't confuse the split; the value is the
/// FIRST whitespace token after it, so an optional trailing timestamp is NOT mistaken for the counter
/// (that would record epoch-millis as the token count — a wildly wrong number). Returns None for a
/// non-sample line.
fn parse_sample(line: &str) -> Option<(&str, Option<String>, u64)> {
    let (ident, remainder) = if let Some(open) = line.find('{') {
        let close = open + line[open..].find('}')?;
        (&line[..=close], &line[close + 1..])
    } else {
        let ws = line.find(char::is_whitespace)?;
        (&line[..ws], &line[ws..])
    };
    // First token after the identifier = the value; ignore any trailing timestamp.
    let value = remainder.split_whitespace().next()?;
    // Prometheus encodes counters as floats; accept "12345" or "12345.0".
    let val = match value.parse::<f64>() {
        Ok(v) if v >= 0.0 && v.is_finite() => v as u64,
        _ => return None,
    };
    let (name, model) = split_name_labels(ident);
    Some((name, model, val))
}

/// Parse a Prometheus text exposition into per-model token counters. Recognizes vLLM
/// (`vllm:prompt_tokens_total` / `vllm:generation_tokens_total`, labeled `model_name`) and
/// llama.cpp (`llamacpp:prompt_tokens_total` / `llamacpp:tokens_predicted_total`, unlabeled ->
/// keyed by ""). Comments, HELP/TYPE lines, and unrelated metrics are ignored.
pub fn parse_token_counters(text: &str) -> BTreeMap<String, TokenCounters> {
    let mut out: BTreeMap<String, TokenCounters> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, model, val)) = parse_sample(line) else {
            continue;
        };
        let (is_prompt, is_gen) = classify(name);
        if !is_prompt && !is_gen {
            continue;
        }
        let e = out.entry(model.unwrap_or_default()).or_default();
        if is_prompt {
            e.prompt = val;
        } else {
            e.generation = val;
        }
    }
    out
}

/// Per-interval token deltas per model between two scrapes. A counter that went DOWN (the server
/// restarted and reset to 0) contributes its full current value, never a negative. Models with no
/// change are omitted.
pub fn counter_deltas(
    prev: &BTreeMap<String, TokenCounters>,
    cur: &BTreeMap<String, TokenCounters>,
) -> BTreeMap<String, TokenCounters> {
    let mut out = BTreeMap::new();
    for (model, c) in cur {
        let p = prev.get(model).copied().unwrap_or_default();
        let d = TokenCounters {
            prompt: c.prompt.checked_sub(p.prompt).unwrap_or(c.prompt),
            generation: c
                .generation
                .checked_sub(p.generation)
                .unwrap_or(c.generation),
        };
        if d.prompt > 0 || d.generation > 0 {
            out.insert(model.clone(), d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VLLM: &str = r#"
# HELP vllm:prompt_tokens_total Number of prefill tokens processed.
# TYPE vllm:prompt_tokens_total counter
vllm:prompt_tokens_total{model_name="google/gemma-2-9b"} 12345.0
vllm:generation_tokens_total{model_name="google/gemma-2-9b"} 6789
vllm:prompt_tokens_total{model_name="meta-llama/Llama-3-8B"} 1000
vllm:num_requests_running{model_name="google/gemma-2-9b"} 2
"#;

    #[test]
    fn parses_vllm_per_model_counters_ignoring_other_metrics() {
        let m = parse_token_counters(VLLM);
        assert_eq!(m.len(), 2);
        let g = m.get("google/gemma-2-9b").unwrap();
        assert_eq!((g.prompt, g.generation), (12345, 6789));
        assert_eq!(m.get("meta-llama/Llama-3-8B").unwrap().prompt, 1000);
    }

    #[test]
    fn ignores_an_optional_trailing_timestamp_not_reading_it_as_the_value() {
        // Prometheus allows `metric value timestamp`. The value is 12345 — the epoch-millis timestamp
        // must NOT be read as the token count (that would be a wildly wrong number).
        let txt = "vllm:prompt_tokens_total{model_name=\"g\"} 12345 1699999999000\n\
                   vllm:generation_tokens_total{model_name=\"g\"} 42 1699999999000\n";
        let c = parse_token_counters(txt).remove("g").unwrap();
        assert_eq!((c.prompt, c.generation), (12345, 42));
    }

    #[test]
    fn takes_the_label_set_structurally_so_a_spaced_label_value_is_safe() {
        // A label value with a space (or an extra label) must not confuse the name/value split.
        let txt = "vllm:prompt_tokens_total{engine=\"v 1\",model_name=\"m\"} 7\n";
        assert_eq!(parse_token_counters(txt).get("m").unwrap().prompt, 7);
    }

    #[test]
    fn parses_unlabeled_llamacpp_counters() {
        let txt = "llamacpp:prompt_tokens_total 500\nllamacpp:tokens_predicted_total 250\n";
        let m = parse_token_counters(txt);
        let c = m.get("").unwrap();
        assert_eq!((c.prompt, c.generation), (500, 250));
    }

    #[test]
    fn deltas_subtract_and_handle_counter_reset() {
        let prev = parse_token_counters(VLLM);
        // gemma advanced; llama restarted (reset below previous).
        let cur_txt = r#"
vllm:prompt_tokens_total{model_name="google/gemma-2-9b"} 12545
vllm:generation_tokens_total{model_name="google/gemma-2-9b"} 6800
vllm:prompt_tokens_total{model_name="meta-llama/Llama-3-8B"} 30
"#;
        let cur = parse_token_counters(cur_txt);
        let d = counter_deltas(&prev, &cur);
        let g = d.get("google/gemma-2-9b").unwrap();
        assert_eq!((g.prompt, g.generation), (200, 11));
        // reset: cur (30) < prev (1000) -> contributes full current value.
        assert_eq!(d.get("meta-llama/Llama-3-8B").unwrap().prompt, 30);
    }

    #[test]
    fn no_change_yields_no_delta_entry() {
        let m = parse_token_counters("vllm:prompt_tokens_total{model_name=\"x\"} 5\n");
        assert!(counter_deltas(&m, &m).is_empty());
    }
}
