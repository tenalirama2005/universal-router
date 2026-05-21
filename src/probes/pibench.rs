// src/probes/pibench.rs
// Capability: policy-constrained tool use with structured decision recording.
//
// Structural fingerprints (from analysis of the Pi-Bench specialist's
// message_send + handle_bootstrap):
//   - params.message.parts[0] has a `data` field (not `text`, not `file`)
//   - data either has `bootstrap: true` (session init) OR has
//     `context_id` + `messages` (array of role/content turn objects)
//   - data carries a `tools` array whose entries follow OpenAI
//     function-calling schema (function.name, function.description,
//     function.parameters)
//
// The discriminator is the COMBINATION of (data part) + (tools schema with
// OpenAI function-calling shape) + (messages array OR bootstrap flag).
// A future track using the same policy-tooluse shape would route here,
// which is the correct generalization. Notably, we do NOT look at the
// specific tool names — that would tie the probe to specific benchmarks.

use serde_json::Value;
use crate::probe::{CapabilityProbe, Upstream};

pub struct PiBenchProbe {
    upstream: Upstream,
}

impl PiBenchProbe {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }
}

impl CapabilityProbe for PiBenchProbe {
    fn name(&self) -> &'static str {
        "policy-tooluse"
    }

    fn score(&self, req: &Value) -> f32 {
        let Some(parts) = req
            .pointer("/params/message/parts")
            .and_then(|v| v.as_array())
        else {
            return 0.0;
        };

        // Pi-Bench uses parts[0].data exclusively; if the first part
        // isn't a data part, this isn't us.
        let Some(first) = parts.first() else {
            return 0.0;
        };
        let p = first.get("root").unwrap_or(first);
        let Some(data) = p.get("data") else {
            return 0.0;
        };

        let mut signal: f32 = 0.0;

        // Bootstrap-shaped init turn
        if data
            .get("bootstrap")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            signal = signal.max(0.7);
        }

        // Continuation turn: context_id + messages array
        let has_context_id = data
            .get("context_id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let has_messages = data
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false);

        if has_context_id && has_messages {
            signal = signal.max(0.65);
        }

        // Tools array with OpenAI function-calling schema fingerprint.
        // This is the strongest single signal — boost when present.
        let tools_schema_match = data
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|tools| {
                tools.iter().any(|t| {
                    t.pointer("/function/name").is_some()
                        && t.pointer("/function/parameters").is_some()
                })
            })
            .unwrap_or(false);

        if tools_schema_match {
            signal = (signal + 0.25).min(1.0);
        }

        // Benchmark_context array (Pi-Bench-specific framing field,
        // but it's a generic "supply prior context as structured nodes"
        // shape — not benchmark-name keyed).
        if data
            .get("benchmark_context")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            signal = (signal + 0.1).min(1.0);
        }

        signal
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}
