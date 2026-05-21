// src/probes/netarena.rs
// Capability: graph-data transformation via generated code.
//
// Structural fingerprints (from analysis of the NetArena specialist's
// extract_query_text + extract_task_fields):
//   - parts[] contains a text part (kind="text" or type="text") with
//     non-empty text content
//   - OR parts[].data.query is a non-empty string
//   - parts[] does NOT contain a file part (archives, images) — pure
//     text-in, text-out
//   - parts[] does NOT contain a data part with `bootstrap`, `messages`,
//     `tools`, or `exit_code` (those belong to other capabilities)
//
// This probe scores on "text-only request envelope with no other
// capability-specific signals." It is intentionally the WEAKEST probe
// because text-in/text-out is the most generic shape — the router will
// only route here when no stronger probe matches.
//
// We do NOT match on keywords like "graph" or "process_graph" in the
// text body. That would be benchmark-name keying through the back door.
// The structural argument is: when the envelope is text-only and no
// other probe claims it, code-gen-against-text-prompt is the catch.

use serde_json::Value;
use crate::probe::{CapabilityProbe, Upstream};

pub struct NetArenaProbe {
    upstream: Upstream,
}

impl NetArenaProbe {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }
}

impl CapabilityProbe for NetArenaProbe {
    fn name(&self) -> &'static str {
        "text-codegen"
    }

    fn score(&self, req: &Value) -> f32 {
        let Some(parts) = req
            .pointer("/params/message/parts")
            .and_then(|v| v.as_array())
        else {
            return 0.0;
        };

        if parts.is_empty() {
            return 0.0;
        }

        let mut has_text = false;
        let mut has_disqualifying = false;

        for part in parts {
            let p = part.get("root").unwrap_or(part);
            let kind = p
                .get("kind")
                .or_else(|| p.get("type"))
                .and_then(|k| k.as_str())
                .unwrap_or("");

            match kind {
                "text" => {
                    if p.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                    {
                        has_text = true;
                    }
                }
                "file" => {
                    // File payloads belong to vuln-repro or vision tracks.
                    has_disqualifying = true;
                }
                "data" => {
                    if let Some(data) = p.get("data") {
                        // Disqualifying data shapes: any other probe's signal
                        if data.get("bootstrap").is_some()
                            || data.get("messages").is_some()
                            || data.get("tools").is_some()
                            || data.get("exit_code").is_some()
                            || data.get("context_id").is_some()
                        {
                            has_disqualifying = true;
                        }
                        // Soft signal: data.query (string) is NetArena's
                        // alternate text-in path
                        if data
                            .get("query")
                            .and_then(|v| v.as_str())
                            .map(|s| !s.is_empty())
                            .unwrap_or(false)
                        {
                            has_text = true;
                        }
                    }
                }
                _ => {}
            }
        }

        if has_disqualifying {
            return 0.0;
        }
        if !has_text {
            return 0.0;
        }

        // Just barely above threshold — this is the weak default for
        // text-only envelopes. Stronger probes (vision, vuln-repro,
        // policy-tooluse) will outscore us when their signals are present.
        0.45
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}
