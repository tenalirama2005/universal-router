// src/probes/fwa.rs
// Capability: visual-document grounded question answering.
//
// Discriminator (RDI-compliant — structural fingerprint, NO benchmark
// names or task IDs):
//
//   A vision-QA task is (image file part) + (text question part) and
//   nothing else. It is single-shot QA over a visual document.
//
//   This shape OVERLAPS with a GUI-automation task, which also carries an
//   image + text. To avoid claiming GUI work, this probe YIELDS (score 0)
//   the moment it sees the structural GUI fingerprint: a DataPart with an
//   `env_config` / `action_space` desktop-control contract, or a desktop
//   observation channel (accessibility_tree / terminal). Same defensive
//   pattern already used to yield archive files to vuln-repro.

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

/// GUI desktop-control fingerprint — if present, this is gui-agent work,
/// not vision-QA. Kept in sync with osworld.rs.
fn has_gui_fingerprint(data: &Value) -> bool {
    let action_space = |obj: &Value| obj.get("action_space").is_some();
    if let Some(env) = data.get("env_config") {
        if action_space(env) {
            return true;
        }
    }
    action_space(data)
        || data.get("accessibility_tree").is_some()
        || data.get("a11y_tree").is_some()
        || data.get("terminal").is_some()
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
        let mut has_gui = false;

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

            if kind == "data" {
                if let Some(data) = p.get("data") {
                    if has_gui_fingerprint(data) {
                        has_gui = true;
                    }
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
        if has_gui {
            return 0.0; // gui-agent territory — do not claim it
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
