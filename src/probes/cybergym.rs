// src/probes/cybergym.rs
// Capability: vulnerability reproduction.
//
// Structural fingerprints (from analysis of the green-agent payload that
// reaches our v82 specialist):
//   - parts[] contains a file part with an archive-shaped name
//     (*.tar.gz pattern) — vuln-repro tasks ship the source tree this
//     way; no other track does
//   - OR parts[] contains a file part named like a patch file
//     (*.diff / *.patch) — level-3 tasks include the fix patch
//   - OR parts[] contains a data part with an `exit_code` field — this
//     is a feedback continuation, which only the vuln-repro workflow
//     uses (test the PoC, return exit code + sanitizer output, ask for
//     the next iteration)
//
// None of these signals are benchmark-name keyed. A future track that
// happened to involve "ship a source archive, get back a binary input"
// would also route here, which is the correct generalization.

use serde_json::Value;
use crate::probe::{CapabilityProbe, Upstream};

pub struct CyberGymProbe {
    upstream: Upstream,
}

impl CyberGymProbe {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }
}

impl CapabilityProbe for CyberGymProbe {
    fn name(&self) -> &'static str {
        "vuln-repro"
    }

    fn score(&self, req: &Value) -> f32 {
        let Some(parts) = req
            .pointer("/params/message/parts")
            .and_then(|v| v.as_array())
        else {
            return 0.0;
        };

        let mut signal = 0.0_f32;

        for part in parts {
            // Some clients wrap parts in a `root` envelope; unwrap if present.
            let p = part.get("root").unwrap_or(part);
            let kind = p.get("kind").and_then(|k| k.as_str()).unwrap_or("");

            match kind {
                "file" => {
                    let name = p
                        .get("file")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_lowercase();

                    // Archive name shape: source-tree shipping pattern
                    if name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".tar")
                    {
                        signal = signal.max(0.85);
                    }
                    // Patch file shape: level-3 vuln-repro pattern
                    if name.ends_with(".diff") || name.ends_with(".patch") {
                        signal = signal.max(0.75);
                    }
                }
                "data" => {
                    // Feedback continuation: data part with exit_code field is
                    // unique to the test-PoC-then-iterate workflow.
                    if let Some(data) = p.get("data") {
                        if data.get("exit_code").is_some() {
                            signal = signal.max(0.95);
                        }
                    }
                }
                _ => {}
            }
        }

        signal
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}
