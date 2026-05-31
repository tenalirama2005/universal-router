// src/probes/fwa.rs
// Capability: visual-document grounded question answering.
//
// Discriminator (RDI-compliant — structural fingerprint, NO benchmark
// names or task IDs):
//
//   A vision-QA task is (visual-document file part) + (text question
//   part) and nothing else. "Visual document" generalizes beyond raster
//   images to any document the model is expected to ground its answer
//   in: PDFs (e.g. work-instruction sheets), text-format manuals, CSV
//   logs, bounding-box metadata files. All are single-shot QA over a
//   supplied document.
//
//   This shape OVERLAPS with a GUI-automation task, which also carries
//   an image + text. To avoid claiming GUI work, this probe YIELDS
//   (score 0) the moment it sees the structural GUI fingerprint: a
//   DataPart with an `env_config` / `action_space` desktop-control
//   contract, or a desktop observation channel (accessibility_tree /
//   terminal). Same defensive pattern already used to yield archive
//   files to vuln-repro.

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
        let mut has_document = false;
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

                // Raster image — strongest vision-QA file signal.
                if mime.starts_with("image/")
                    || name.ends_with(".png")
                    || name.ends_with(".jpg")
                    || name.ends_with(".jpeg")
                    || name.ends_with(".webp")
                {
                    has_image = true;
                }

                // Visual document — supplied source of truth the model
                // must ground its answer in. PDFs and text-format
                // manuals/logs/metadata all count.
                if mime == "application/pdf"
                    || name.ends_with(".pdf")
                    || mime.starts_with("text/")
                    || name.ends_with(".txt")
                    || name.ends_with(".csv")
                    || name.ends_with(".md")
                    || name.ends_with(".json")
                    // Bounding-box metadata files (factory/warehouse
                    // safety scenes ship these alongside an image)
                    || name.contains("bounding_box")
                {
                    has_document = true;
                }

                // Archive shape — yield to vuln-repro.
                if name.ends_with(".tar.gz")
                    || name.ends_with(".tgz")
                    || name.ends_with(".tar")
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

        // Image is the canonical signal; document (PDF/text/etc.) is
        // also a vision-QA signal at slightly lower confidence. Both
        // require a text question part to be a proper QA envelope.
        match (has_image, has_document, has_text) {
            (true, _, true)      => 0.9,   // image + question — strongest
            (true, _, false)     => 0.6,   // image only — weak QA signal
            (false, true, true)  => 0.75,  // document (PDF/text) + question
            (false, true, false) => 0.4,   // document only — very weak
            _                    => 0.0,
        }
    }

    fn upstream(&self) -> &Upstream {
        &self.upstream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ResponseShape;
    use serde_json::json;

    fn up() -> Upstream {
        Upstream { url: "http://test".into(), response_shape: ResponseShape::Json }
    }

    fn build(parts: Value) -> Value {
        json!({"params": {"message": {"parts": parts}}})
    }

    #[test]
    fn fwa_image_plus_text_still_scores_high() {
        let probe = FwaProbe::new(up());
        let req = build(json!([
            {"kind": "text", "text": "How many workers?"},
            {"kind": "file", "file": {"name": "scene.jpg", "mimeType": "image/jpeg"}}
        ]));
        let s = probe.score(&req);
        assert!(s > 0.85, "image+text should score ≥0.85, got {s}");
    }

    #[test]
    fn fwa_pdf_plus_text_now_scores_above_threshold() {
        let probe = FwaProbe::new(up());
        let req = build(json!([
            {"kind": "text", "text": "List the items in this checklist."},
            {"kind": "file", "file": {"name": "manual.pdf", "mimeType": "application/pdf"}}
        ]));
        let s = probe.score(&req);
        assert!(s >= 0.7, "pdf+text must clear 0.35 threshold with margin, got {s}");
    }

    #[test]
    fn fwa_txt_plus_text_now_scores_above_threshold() {
        let probe = FwaProbe::new(up());
        let req = build(json!([
            {"kind": "text", "text": "What are the business hours?"},
            {"kind": "file", "file": {"name": "Store_Manual_A.txt", "mimeType": "text/plain"}}
        ]));
        let s = probe.score(&req);
        assert!(s >= 0.7, "txt+text must clear 0.35 threshold with margin, got {s}");
    }

    #[test]
    fn fwa_yields_archive_to_vuln_repro() {
        let probe = FwaProbe::new(up());
        let req = build(json!([
            {"kind": "text", "text": "Analyze this."},
            {"kind": "file", "file": {"name": "src.tar.gz", "mimeType": "application/gzip"}}
        ]));
        assert_eq!(probe.score(&req), 0.0);
    }

    #[test]
    fn fwa_yields_gui_to_osworld() {
        let probe = FwaProbe::new(up());
        let req = build(json!([
            {"kind": "text", "text": "Open the file manager."},
            {"kind": "data", "data": {"env_config": {"action_space": "pyautogui"}}},
            {"kind": "file", "file": {"name": "screen.png", "mimeType": "image/png"}}
        ]));
        assert_eq!(probe.score(&req), 0.0);
    }

    #[test]
    fn fwa_text_only_does_not_claim_text_codegen() {
        // Pure text-only payload — NetArena territory, FWA must yield.
        let probe = FwaProbe::new(up());
        let req = build(json!([
            {"kind": "text", "text": "Write a function to process this graph."}
        ]));
        assert_eq!(probe.score(&req), 0.0);
    }
}
