// src/probes/fwa.rs
// Capability: visual-document grounded question answering.
//
// STUB — fingerprints inferred from FWA Sprint 2/3 history (Qwen2.5-VL,
// image inputs, question-answer task shape). Final scoring rules will be
// tuned once a2a/handler.rs is shared and a real captured payload can be
// inspected.
//
// Expected structural fingerprints:
//   - parts[] contains a file part with an image-shaped MIME type
//     (image/png, image/jpeg) OR an image-shaped filename (.png/.jpg)
//   - AND parts[] contains a text part carrying the question
//   - parts[] does NOT contain archive files (that's vuln-repro) or
//     OpenAI tool schemas (that's policy-tooluse)
//
// The combination of (image file part) + (text question part) is the
// generalizable shape — any visual-document QA benchmark fits this.
// We do NOT match on "FWA", FieldWorkArena URLs, or task IDs.

use serde_json::Value;
use crate::probe::{CapabilityProbe, Upstream};

pub struct FwaProbe {
    upstream: Upstream,
}

impl FwaProbe {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }
}

impl CapabilityProbe for FwaProbe {
    fn name(&self) -> &'static str {
        "vision-qa"
    }

    fn score(&self, req: &Value) -> f32 {
        let Some(parts) = req
            .pointer("/params/message/parts")
            .and_then(|v| v.as_array())
        else {
            return 0.0;
        };

        let mut has_image = false;
        let mut has_text = false;
        let mut has_archive = false;

        for part in parts {
            let p = part.get("root").unwrap_or(part);
            let kind = p.get("kind").and_then(|k| k.as_str()).unwrap_or("");

            if kind == "file" {
                let file = p.get("file");
                let mime = file
                    .and_then(|f| f.get("mimeType"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let name = file
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_lowercase();

                if mime.starts_with("image/")
                    || name.ends_with(".png")
                    || name.ends_with(".jpg")
                    || name.ends_with(".jpeg")
                    || name.ends_with(".webp")
                {
                    has_image = true;
                }
                if name.ends_with(".tar.gz")
                    || name.ends_with(".tgz")
                    || name.ends_with(".diff")
                    || name.ends_with(".patch")
                {
                    has_archive = true;
                }
            }

            if kind == "text"
                && p.get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
            {
                has_text = true;
            }
        }

        if has_archive {
            return 0.0; // vuln-repro territory
        }

        match (has_image, has_text) {
            (true, true) => 0.9,
            (true, false) => 0.6, // image only, weak vision-QA signal
            _ => 0.0,
        }
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}
