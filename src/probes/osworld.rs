// src/probes/osworld.rs
// Capability: GUI agent — screenshot in, coordinate action out.
//
// Discriminator (RDI-compliant — structural fingerprint, NO benchmark
// names, task IDs, or content-keyword tables):
//
//   A GUI-automation task carries a DataPart whose `data` object contains
//   `env_config` with an `action_space` (e.g. "pyautogui") and an
//   `observation_type` (e.g. "screenshot"). This is the environment
//   contract for a desktop-control loop — it describes HOW the agent acts
//   on a machine, which no question-answering task has. It generalizes to
//   any GUI/computer-use benchmark that declares an action space; it is
//   not tied to OSWorld by name.
//
//   Secondary structural signals (accessibility_tree / terminal DataParts,
//   a recurring context_id) reinforce but are not required.

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

/// True if `data` is an env/action-space contract for desktop control.
/// Matches both nested (`data.env_config.action_space`) and flat
/// (`data.action_space`) shapes for robustness across green versions.
fn is_gui_env_config(data: &Value) -> bool {
    let probe = |obj: &Value| {
        obj.get("action_space").is_some()
            && (obj.get("observation_type").is_some()
                || obj.get("screen_size").is_some()
                || obj.get("observation").is_some())
    };
    if let Some(env) = data.get("env_config") {
        if probe(env) {
            return true;
        }
    }
    probe(data)
}

/// True if `data` carries a desktop-observation channel.
fn is_desktop_observation(data: &Value) -> bool {
    data.get("accessibility_tree").is_some()
        || data.get("a11y_tree").is_some()
        || data.get("terminal").is_some()
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
        let mut has_env_config = false;
        let mut has_desktop_obs = false;

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
                    if is_gui_env_config(data) {
                        has_env_config = true;
                    }
                    if is_desktop_observation(data) {
                        has_desktop_obs = true;
                    }
                }
            }
        }

        // The env_config/action_space DataPart is the definitive GUI
        // fingerprint. Everything else only reinforces it.
        if has_env_config {
            return if has_image || has_desktop_obs { 0.97 } else { 0.90 };
        }

        // Fallback: a desktop observation channel (a11y tree / terminal)
        // alongside a screenshot is still a strong GUI signal even if the
        // env_config part is absent in some green-agent version.
        if has_desktop_obs && has_image {
            return 0.75;
        }

        0.0
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::probes::fwa::FwaProbe;
    use crate::probe::ResponseShape;
    use serde_json::json;

    fn up() -> Upstream {
        Upstream { url: "http://test".into(), response_shape: ResponseShape::Json }
    }

    // Real OSWorld message/send body — matches osworld-green agent.py:
    // TextPart(instruction) + DataPart(env_config) + FilePart(image/png).
    fn osworld_payload() -> serde_json::Value {
        json!({
            "params": { "message": {
                "kind": "message", "role": "user", "messageId": "abc",
                "contextId": "ctx-1",
                "parts": [
                    { "kind": "text", "text": "Open the file manager and create a folder named Reports." },
                    { "kind": "data", "data": { "env_config": {
                        "action_space": "pyautogui",
                        "observation_type": "screenshot"
                    }}},
                    { "kind": "file", "file": { "bytes": "iVBORw0KGgo=", "mimeType": "image/png" }}
                ]
            }}
        })
    }

    fn fwa_payload() -> serde_json::Value {
        json!({
            "params": { "message": {
                "kind": "message", "role": "user", "messageId": "x",
                "parts": [
                    { "kind": "text", "text": "How many workers are wearing helmets?" },
                    { "kind": "file", "file": { "bytes": "iVBORw0KGgo=", "mimeType": "image/jpeg" }}
                ]
            }}
        })
    }

    #[test]
    fn osworld_routes_to_gui_agent() {
        let osw = OsworldProbe::new(up());
        let fwa = FwaProbe::new(up());
        let p = osworld_payload();
        let osw_score = osw.score(&p);
        let fwa_score = fwa.score(&p);
        println!("OSWorld payload -> gui-agent={osw_score:.2} vision-qa={fwa_score:.2}");
        assert!(osw_score > 0.9, "gui-agent must score high, got {osw_score}");
        assert_eq!(fwa_score, 0.0, "vision-qa must yield, got {fwa_score}");
        assert!(osw_score > fwa_score);
    }

    #[test]
    fn fwa_still_routes_to_vision_qa() {
        let osw = OsworldProbe::new(up());
        let fwa = FwaProbe::new(up());
        let p = fwa_payload();
        let osw_score = osw.score(&p);
        let fwa_score = fwa.score(&p);
        println!("FWA payload -> gui-agent={osw_score:.2} vision-qa={fwa_score:.2}");
        assert_eq!(osw_score, 0.0, "gui-agent must not claim vision-QA");
        assert!(fwa_score > 0.8, "vision-qa must still win, got {fwa_score}");
    }
}