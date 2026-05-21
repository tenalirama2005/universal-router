// src/probes/osworld.rs
// Capability: GUI agent — screenshot in, action out.
//
// STUB — fingerprints inferred from OSWorld-Verified general structure
// (screenshot + action schema, coordinate-shaped actions). Final scoring
// rules will be tuned once the OSWorld green-agent code is fetched from
// GitHub and a real captured payload can be inspected.
//
// Expected structural fingerprints:
//   - parts[] contains a file part with a large image (screenshot —
//     typically 1024x768+ at the OSWorld resolution)
//   - AND parts[] contains a data part with an action schema or
//     allowed-actions list
//   - OR parts[] contains a text part describing the task plus an image
//
// The discriminator from FWA (which also has image+text) is the presence
// of an action schema or coordinate-action shape in the metadata —
// OSWorld is "do something on a desktop" not "answer a question about
// an image."

use serde_json::Value;
use crate::probe::{CapabilityProbe, Upstream};

pub struct OsworldProbe {
    upstream: Upstream,
}

impl OsworldProbe {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }
}

impl CapabilityProbe for OsworldProbe {
    fn name(&self) -> &'static str {
        "gui-agent"
    }

    fn score(&self, req: &Value) -> f32 {
        let Some(parts) = req
            .pointer("/params/message/parts")
            .and_then(|v| v.as_array())
        else {
            return 0.0;
        };

        let mut has_image = false;
        let mut has_action_schema = false;

        for part in parts {
            let p = part.get("root").unwrap_or(part);
            let kind = p.get("kind").and_then(|k| k.as_str()).unwrap_or("");

            if kind == "file" {
                let mime = p
                    .pointer("/file/mimeType")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let name = p
                    .pointer("/file/name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_lowercase();

                if mime.starts_with("image/")
                    || name.ends_with(".png")
                    || name.ends_with(".jpg")
                    || name.ends_with(".jpeg")
                {
                    has_image = true;
                }
            }

            if kind == "data" {
                if let Some(data) = p.get("data") {
                    // Action-schema fingerprint: presence of `action_space`,
                    // `available_actions`, `actions`, or similar coordinate-
                    // shaped action descriptors.
                    if data.get("action_space").is_some()
                        || data.get("available_actions").is_some()
                        || data.get("actions").is_some()
                        || data.get("screen_size").is_some()
                        || data.get("observation").is_some()
                    {
                        has_action_schema = true;
                    }
                }
            }
        }

        match (has_image, has_action_schema) {
            (true, true) => 0.95,
            (false, true) => 0.7, // action schema alone is still strong
            _ => 0.0,
        }
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}
