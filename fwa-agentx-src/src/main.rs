// src/main.rs — fwa-agentx v1 (Sprint 4)
// Architecture: Rust/Axum A2A server (pattern: cybergym-agentx-v2)
// LLM: GPT-5.4 (pinned to 2026-03-05) primary, GPT-5.5 fallback
//
// Endpoint shape (A2A JSON-RPC):
//   POST /                       — JSON-RPC task envelope
//   POST /a2a/tasks/send         — alias
//   POST /agent  +  /agent/      — alias
//   GET  /.well-known/agent-card.json
//   GET  /health
//
// Multimodal parts handled:
//   - TextPart                                  → user message text
//   - FilePart with image/* mime                → OpenAI vision image_url (base64 data URI)
//   - FilePart with application/pdf             → extracted text via pdf-extract
//   - FilePart filename contains "Bounding_Box" → text content
//   - FilePart with text/* mime                 → text content
//   - FilePart with video/mp4 mime              → frames extracted via ffmpeg subprocess (best-effort)
//
// Response: A2A SSE event stream with task → status-update(working) → artifact-update(text) → status-update(completed,final)
//
// FWA-specific: single-shot inference (no iteration), no workspace dir, no feedback loop.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use base64::Engine;
use bytes::Bytes;
use clap::Parser;
use futures::stream;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

const DEFAULT_PORT: u16 = 8090;
const OPENAI_DIRECT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

// Pinned model snapshot (matches what we verified in the OpenAI smoke test).
// Configurable via FWA_MODEL_PRIORITY env var as comma-separated list.
const DEFAULT_MODEL_PRIORITY: &str = "gpt-5.4-2026-03-05,gpt-5.4,gpt-5.5";
const MAX_COMPLETION_TOKENS: u64 = 4096;
const MIN_COMPLETION_TOKENS: u64 = 1;

const RETRY_BASE_DELAY_MS: u64 = 1000;
const RETRY_MAX_ATTEMPTS: u32 = 3;

// ─── v2: Task-type inference + per-type prompt addenda ────────────────────
//
// Rationale (from FWA run 71642054400 analysis):
//   - numerical_match eval_func: 12.5% pass rate (worst)
//   - json_match eval_func:      29.4% pass rate
//   - fuzzy_match:               59.1% pass rate
// ab-shetty's public config uses reasoning_effort="high" for numerical
// and JSON tasks, "medium" otherwise — and we observed reasoning_tokens=0
// across all our calls. This patch matches that signal AND adds JSON
// format discipline to fix the wrapper-key, ID-type, and procedure-
// hallucination bugs identified in the same analysis.

#[derive(Debug, Clone, Copy)]
enum TaskHint {
    JsonReport,   // {"total_violations": N, "details": [...]}
    JsonArray,    // [{...}, {...}]
    Numerical,    // distance / count
    General,      // default (fuzzy_match, descriptions, yes/no)
}

fn infer_task_hint(question: &str) -> TaskHint {
    let q = question.to_lowercase();

    // JSON cues — phrases in tasks with output_format=json
    let json_signals = [
        "json", "create a new issue", "generate a json", "report the findings",
        "structured format", "field \"id\"", "\"id\":",
        "report should begin with the total",
    ];
    if json_signals.iter().any(|s| q.contains(s)) {
        // JsonReport: wrapped {"total_violations": N, "details": [...]}
        let report_signals = [
            "total number of violations",
            "begin by stating the total",
            "begin with the total",
            "total_violations",
            "if there were violations",
            "if there were incidents",
            "create a new issue for each",
            "for each violations",
            "for each incidents",
        ];
        if report_signals.iter().any(|s| q.contains(s)) {
            return TaskHint::JsonReport;
        }
        return TaskHint::JsonArray;
    }

    let numerical_signals = [
        "how many", "how far", "what is the distance", "what's the distance",
        "less than", " meters", " metres",
    ];
    if numerical_signals.iter().any(|s| q.contains(s)) {
        return TaskHint::Numerical;
    }

    TaskHint::General
}

const JSON_REPORT_ADDENDUM: &str = r#"
TASK-SPECIFIC OUTPUT FORMAT (JSON report with totals):

1. Output a SINGLE JSON object with EXACTLY this shape:
   {"total_violations": <integer>, "details": [<issue objects>]}

2. The wrapper key for the array MUST be "details". NEVER "violations",
   never "issues", never any other name. This is the single most common
   failure mode — get it right.

3. If there are zero violations/incidents, output exactly:
   {"total_violations": 0, "details": []}

4. Each issue object uses these keys (string values, ID as quoted string):
   {"ID": "1120", "Category": "violation", "Short description": "...",
    "Filename": "...", "Description": "..."}
   - ID is ALWAYS a quoted string ("1120", not 1120).
   - If the task asks for "Image filename" instead of "Filename", use the
     EXACT key name the task asked for.
   - If the task does NOT ask for a Description field, omit it.

5. PROCEDURE-COMPLIANCE TASKS (image + procedure PDF):
   A violation requires VISUAL EVIDENCE of non-compliance IN THE IMAGE.
   The procedure document describes what SHOULD be done; it is NOT
   evidence of what WAS done. If the image shows the worker apparently
   performing the task as required, OR if compliance cannot be visually
   confirmed either way, output:
      {"total_violations": 0, "details": []}
   Reading the procedure alone is NEVER sufficient grounds to declare a
   violation. Default to zero unless the image clearly shows the worker
   doing it wrong.

6. MULTI-IMAGE TASKS: examine each image, list per-image findings
   internally, then assemble the combined details array. The
   total_violations count must equal details.len().

7. Output ONLY the JSON object — no ```json fences, no preface, no commentary.
"#;

const JSON_ARRAY_ADDENDUM: &str = r#"
TASK-SPECIFIC OUTPUT FORMAT (JSON array of issues):

1. Output a SINGLE JSON array: [{...}, {...}].
   If there are no issues, output exactly: []

2. Each issue object uses string values, with ID as a quoted string:
   {"ID": "1060", "Category": "violation", "Short description": "...",
    "Image filename": "..."}
   - ID is ALWAYS a quoted string ("1060", not 1060).
   - Use the EXACT key names the task asked for. If the task says
     "Image filename", use "Image filename"; if "Filename", use "Filename".
   - Include "Description" only if the task explicitly lists it.

3. Output ONLY the JSON array — no ```json fences, no preface, no commentary.
"#;

const NUMERICAL_ADDENDUM: &str = r#"
TASK-SPECIFIC OUTPUT FORMAT (numerical answer):

1. State the numerical value with its unit. Use one decimal place for
   distances (e.g., "0.7 meters", "2.5 meters"). Use integers for counts.

2. MULTI-IMAGE COUNTING (e.g. "How many incidents..."):
   - You will receive N images.
   - Examine EACH image independently first.
   - State your per-image conclusion in one short clause before the total.
   - The final count is the sum of per-image positives.
   Example: "Image 1: 0.7m (incident). Image 2: 2.5m (no incident).
   Image 3: 0.8m (incident). There were two incidents."

3. For "Is X less than Y?" questions: answer Yes/No first, then state
   the measured distance. Example: "Yes, it is 0.7 meters."

4. Be conservative on distance estimates. If the question shows
   calibration cues (people, doors, pallets visible at known scale),
   use them.
"#;

// FWA system prompt — distilled from purple_executor.py's distance_guidance
// + a clear instruction to output a concise answer the green agent's
// fuzzy_match scorer can accept.
const FWA_SYSTEM_PROMPT: &str = r#"You are a vision-language analyst for the FieldWorkArena benchmark
(factory and warehouse safety inspection). You will receive a question
plus supporting images, PDFs, text files, and bounding-box metadata.

The benchmark scores your answer with a fuzzy text match against a
reference answer. Reference answers are usually FULL SENTENCES that
restate the question's key terms and explain the verdict — not single
words or numbers in isolation.

Output policy:
- Write a complete answer that RESTATES the relevant terms from the
  question. e.g. for "What are the business hours?" reply
  "Business hours are 08:00 to 20:00." — not "08:00 - 20:00.".
- For yes/no questions about counts/violations/areas, write a multi-clause
  answer that (1) gives the direct yes/no, (2) describes what you see in
  the relevant region of the image, and (3) classifies the situation
  (e.g. "within threshold / not an incident", or "exceeds threshold / this
  is an incident"). Example:
    Q: "Are there 2 or fewer workers in the area defined by ...? If not, identify it as an incident."
    A: "Yes, there are no workers in the designated area. Within threshold. This is not an incident."
- For checklist or instruction questions from PDFs, reproduce the
  checklist using the same headings ("Visual check:", "Tensile check:",
  "Air test check:", "Check for dirt:") and short descriptions.
- For distance questions, give the value AND restate it: "The distance
  is approximately 2.5 meters."
- For counting questions, give the integer AND name the entity counted:
  "There are 3 workers in the image."
- For PPE/compliance questions, name the specific item(s) involved
  AND give the compliance verdict in one or two sentences.
- For questions that ask for a specific output format like JSON, return
  ONLY that format — no markdown fences, no commentary.

Bounding-box / coordinate reasoning:
- When the question gives image-pixel coordinates (e.g. (1330, 680)),
  treat them as defining a small rectangular region in the image and
  describe ONLY what is inside that region.
- If the region is empty of the queried entity, say so explicitly:
  "There are no workers in the designated area."

Distance estimation cues (used only when distance is asked):
- Reference scale: doors ~2m, chairs ~0.5m, people ~1.7m, pallets ~1.2m, shelves ~2m.
- Use floor tiles, ceiling grids, conveyor belts for perspective.
- Industrial equipment has standardized dimensions.

Do not include reasoning steps, prefaces ("Sure,"), or follow-up offers.
Output the answer only. The answer should typically be 1–3 sentences
unless the question asks for a list or document content."#;

// ───────────────────────────────────────────────────────────────────────────
// CLI args
// ───────────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    #[arg(long)]
    card_url: Option<String>,
}

// ───────────────────────────────────────────────────────────────────────────
// App state
// ───────────────────────────────────────────────────────────────────────────

pub struct AppState {
    pub openai_api_key: String,
    pub model_priority: Vec<String>,
    pub min_completion_tokens: u64,
    pub agent_card_url: String,
}

// ───────────────────────────────────────────────────────────────────────────
// main
// ───────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // .env support for OPENAI_API_KEY and friends in dev
    let _ = dotenvy::dotenv();

    // Tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fwa_agentx=info,tower_http=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();

    let openai_api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if openai_api_key.is_empty() {
        warn!("[fwa-agentx] OPENAI_API_KEY not set — model calls will fail.");
    }

    let model_priority_raw = std::env::var("FWA_MODEL_PRIORITY")
        .unwrap_or_else(|_| DEFAULT_MODEL_PRIORITY.to_string());
    let model_priority: Vec<String> = model_priority_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let model_priority = if model_priority.is_empty() {
        DEFAULT_MODEL_PRIORITY
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    } else {
        model_priority
    };

    let min_completion_tokens = std::env::var("FWA_MIN_COMPLETION_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(MIN_COMPLETION_TOKENS);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(args.port);

    let bind_addr = format!("{}:{}", args.host, port);
    let agent_card_url = args
        .card_url
        .clone()
        .unwrap_or_else(|| format!("http://{}/", bind_addr));

    info!(
        "[fwa-agentx] model_priority={:?} min_completion_tokens={} bind={}",
        model_priority, min_completion_tokens, bind_addr
    );

    let state = Arc::new(AppState {
        openai_api_key,
        model_priority,
        min_completion_tokens,
        agent_card_url,
    });

    let app = Router::new()
        .route("/.well-known/agent-card.json", get(agent_card))
        .route("/", post(handle_task))
        .route("/a2a/tasks/send", post(handle_task))
        .route("/agent", post(handle_task))
        .route("/agent/", post(handle_task))
        .route("/health", get(health))
        .route("/healthz", get(health))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind(&bind_addr).await.unwrap();
    info!("[fwa-agentx] listening on {}", bind_addr);
    axum::serve(listener, app).await.unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Endpoints: /health, /agent-card
// ───────────────────────────────────────────────────────────────────────────

async fn health() -> Response {
    let body = json!({"status": "ready", "service": "fwa-agentx", "version": "1.0.0"});
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn agent_card(State(state): State<Arc<AppState>>) -> Response {
    let card = json!({
        "name": "fba_purple_agent",
        "description": "Purple agent for FieldWorkArena — vision-language model inference for factory and warehouse safety analysis.",
        "url": state.agent_card_url,
        "version": "1.0.0",
        "defaultInputModes": ["text", "text/plain", "image/jpeg", "image/png", "video/mp4", "application/pdf"],
        "defaultOutputModes": ["text", "text/plain"],
        "capabilities": {"streaming": true},
        "skills": [{
            "id": "fba_field_work_agent",
            "name": "fba_purple_agent",
            "description": "Vision agent for field work safety analysis.",
            "tags": ["field_work", "vision", "safety", "factory", "warehouse"],
            "examples": [
                "Check PPE compliance status from factory images.",
                "Count safety violations in warehouse video.",
                "Analyze factory incident from camera footage."
            ]
        }]
    });
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        card.to_string(),
    )
        .into_response()
}

// ───────────────────────────────────────────────────────────────────────────
// Endpoint: handle_task — main A2A JSON-RPC entrypoint
// ───────────────────────────────────────────────────────────────────────────

async fn handle_task(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let raw: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("[fwa-agentx] JSON parse error: {}", e);
            return json_rpc_error(Value::Null, -32700, format!("Parse error: {}", e));
        }
    };

    let rpc_id = raw.get("id").cloned().unwrap_or(Value::Null);
    let method = raw.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
    info!("[fwa-agentx] method={} rpc_id={}", method, rpc_id);

    let message = match raw.get("params").and_then(|p| p.get("message")) {
        Some(m) => m.clone(),
        None => {
            warn!("[fwa-agentx] Missing params.message");
            return json_rpc_error(rpc_id, -32602, "Missing params.message".to_string());
        }
    };

    let context_id = message
        .get("contextId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let task_id = Uuid::new_v4().to_string();
    info!("[fwa-agentx] context_id={} task_id={}", context_id, task_id);

    // Extract parts: question text + multimodal files
    let parts_info = extract_fwa_parts(&message);
    info!(
        "[fwa-agentx] parts: question={} chars, images={}, pdfs={}, bbox_texts={}, video_frames={}",
        parts_info.question.len(),
        parts_info.images.len(),
        parts_info.pdf_texts.len(),
        parts_info.bbox_texts.len(),
        parts_info.video_frames.len(),
    );

    // Try to extract task_id from question text (purely for logging)
    let fwa_task_id = extract_task_id(&parts_info.question);
    if let Some(t) = &fwa_task_id {
        info!("[fwa-agentx] FWA task_id from text: {}", t);
    }

    // Branch on method:
    //   - "message/send"   → synchronous: run model call inline, return plain JSON-RPC
    //   - "message/stream" → asynchronous: return SSE event stream
    //   - anything else    → default to synchronous JSON (safer)
    let want_stream = method == "message/stream";

    if !want_stream {
        // ─── Synchronous path: message/send (and default) ───
        // Run the OpenAI call inline; return a single JSON-RPC response with a
        // completed Task object embedding the artifact.
        let task_hint = infer_task_hint(&parts_info.question);
        let model_result = call_openai_vision(
            &state.openai_api_key,
            &state.model_priority,
            state.min_completion_tokens,
            &parts_info,
            task_hint,
        )
        .await;

        let (answer_text, model_used) = match model_result {
            Ok((text, m)) => (text, m),
            Err(e) => {
                warn!("[fwa-agentx] model error: {}", e);
                (format!("ERROR: {}", e), "error".to_string())
            }
        };

        info!(
            "[fwa-agentx] answer (model={}): {:?}",
            model_used,
            safe_slice(&answer_text, 200)
        );

        let task_obj = json!({
            "kind": "task",
            "id": task_id,
            "contextId": context_id,
            "status": {"state": "completed"},
            "artifacts": [{
                "artifactId": Uuid::new_v4().to_string(),
                "name": "answer",
                "parts": [{"kind": "text", "text": answer_text}]
            }],
            "history": []
        });

        let body = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": task_obj
        });
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            body.to_string(),
        )
            .into_response();
    }

    // ─── Streaming path: message/stream ───
    let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(16);
    let state_clone = state.clone();
    let rpc_id_clone = rpc_id.clone();
    let task_id_clone = task_id.clone();
    let context_id_clone = context_id.clone();

    tokio::spawn(async move {
        // Notify: working
        let _ = sse_tx
            .send(sse_event_bytes(json!({
                "jsonrpc": "2.0", "id": rpc_id_clone,
                "result": {
                    "kind": "status-update",
                    "taskId": task_id_clone,
                    "contextId": context_id_clone,
                    "status": {"state": "working"}
                }
            })))
            .await;

        let task_hint = infer_task_hint(&parts_info.question);
        let model_result = call_openai_vision(
            &state_clone.openai_api_key,
            &state_clone.model_priority,
            state_clone.min_completion_tokens,
            &parts_info,
            task_hint,
        )
        .await;

        let (answer_text, model_used) = match model_result {
            Ok((text, model)) => (text, model),
            Err(e) => {
                warn!("[fwa-agentx] model error: {}", e);
                (format!("ERROR: {}", e), "error".to_string())
            }
        };

        info!(
            "[fwa-agentx] answer (model={}): {:?}",
            model_used,
            safe_slice(&answer_text, 200)
        );

        let _ = sse_tx
            .send(sse_event_bytes(json!({
                "jsonrpc": "2.0", "id": rpc_id_clone,
                "result": {
                    "kind": "artifact-update",
                    "taskId": task_id_clone,
                    "contextId": context_id_clone,
                    "artifact": {
                        "artifactId": Uuid::new_v4().to_string(),
                        "name": "answer",
                        "parts": [
                            {"kind": "text", "text": answer_text}
                        ]
                    },
                    "append": false,
                    "lastChunk": true
                }
            })))
            .await;

        let _ = sse_tx
            .send(sse_event_bytes(json!({
                "jsonrpc": "2.0", "id": rpc_id_clone,
                "result": {
                    "kind": "status-update",
                    "taskId": task_id_clone,
                    "contextId": context_id_clone,
                    "status": {"state": "completed"},
                    "final": true
                }
            })))
            .await;
    });

    let initial_event = sse_event_bytes(json!({
        "jsonrpc": "2.0", "id": rpc_id,
        "result": {
            "kind": "task",
            "id": task_id,
            "contextId": context_id,
            "status": {"state": "submitted"}
        }
    }));

    let stream = stream::unfold(
        (Some(initial_event), sse_rx),
        |(initial, mut rx)| async move {
            if let Some(e) = initial {
                return Some((Ok::<bytes::Bytes, std::convert::Infallible>(e), (None, rx)));
            }
            match rx.recv().await {
                Some(event) => Some((Ok(event), (None, rx))),
                None => None,
            }
        },
    );

    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    headers.insert("x-accel-buffering", "no".parse().unwrap());
    headers.insert("transfer-encoding", "chunked".parse().unwrap());
    headers.insert("connection", "keep-alive".parse().unwrap());
    (StatusCode::OK, headers, Body::from_stream(stream)).into_response()
}

/// Helper: build a plain JSON-RPC error response (not SSE).
fn json_rpc_error(rpc_id: Value, code: i32, message: String) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "error": {"code": code, "message": message}
    });
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

// ───────────────────────────────────────────────────────────────────────────
// FWA parts extraction
// ───────────────────────────────────────────────────────────────────────────

#[derive(Default, Debug)]
struct FwaParts {
    pub question: String,
    /// (filename, jpeg_bytes) — already converted to JPEG
    pub images: Vec<(String, Vec<u8>)>,
    pub pdf_texts: Vec<(String, String)>,
    pub bbox_texts: Vec<(String, String)>,
    pub text_files: Vec<(String, String)>,
    /// Frames from videos as JPEG bytes — best-effort via ffmpeg subprocess
    pub video_frames: Vec<(String, Vec<u8>)>,
}

fn extract_fwa_parts(message: &Value) -> FwaParts {
    let mut out = FwaParts::default();

    let parts = match message.get("parts").and_then(|p| p.as_array()) {
        Some(arr) => arr,
        None => return out,
    };

    for part in parts {
        // Two A2A part shapes:
        //   - { "kind": "text", "text": "..." }
        //   - { "kind": "file", "file": {"name": "...", "mimeType": "...", "bytes": "<base64>"} }
        let kind = part
            .get("kind")
            .and_then(|v| v.as_str())
            .or_else(|| part.get("type").and_then(|v| v.as_str()))
            .unwrap_or("");

        match kind {
            "text" => {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    if !out.question.is_empty() {
                        out.question.push('\n');
                    }
                    out.question.push_str(t);
                }
            }
            "file" => {
                let file = match part.get("file") {
                    Some(f) => f,
                    None => continue,
                };
                let name = file
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("attachment")
                    .to_string();
                let mime = file
                    .get("mimeType")
                    .or_else(|| file.get("mime_type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Decode bytes — A2A supports either "bytes": "<base64>" or "uri": "..."
                let bytes_field = file.get("bytes").and_then(|v| v.as_str());
                let decoded: Option<Vec<u8>> = bytes_field.and_then(|b64| {
                    let cleaned = if let Some(idx) = b64.find(',') {
                        if b64[..idx].starts_with("data:") {
                            &b64[idx + 1..]
                        } else {
                            b64
                        }
                    } else {
                        b64
                    };
                    b64_decode(cleaned).ok()
                });

                let data = match decoded {
                    Some(d) => d,
                    None => {
                        warn!(
                            "[fwa-agentx] file {} ({}): no decodable bytes — skipping",
                            name, mime
                        );
                        continue;
                    }
                };

                info!(
                    "[fwa-agentx] file {} ({}): {} bytes",
                    name,
                    mime,
                    data.len()
                );

                // Routing — match Python's purple_executor.py order
                let name_lower = name.to_lowercase();

                if name_lower.contains("bounding_box") || name_lower.contains("bbox") {
                    let text = String::from_utf8_lossy(&data).to_string();
                    out.bbox_texts.push((name, text));
                    continue;
                }

                if mime.starts_with("image/")
                    || name_lower.ends_with(".jpg")
                    || name_lower.ends_with(".jpeg")
                    || name_lower.ends_with(".png")
                    || name_lower.ends_with(".webp")
                {
                    match reencode_image_to_jpeg(&data) {
                        Ok(jpeg) => out.images.push((name, jpeg)),
                        Err(e) => {
                            warn!("[fwa-agentx] image decode error for {}: {}", name, e);
                        }
                    }
                    continue;
                }

                if mime == "application/pdf" || name_lower.ends_with(".pdf") {
                    match extract_pdf_text(&data) {
                        Ok(text) => out.pdf_texts.push((name, text)),
                        Err(e) => warn!("[fwa-agentx] pdf decode error for {}: {}", name, e),
                    }
                    continue;
                }

                if mime.starts_with("video/") || name_lower.ends_with(".mp4") {
                    match extract_video_frames(&data, 8) {
                        Ok(frames) => {
                            for (i, frame) in frames.into_iter().enumerate() {
                                out.video_frames.push((format!("{}_frame_{}", name, i), frame));
                            }
                        }
                        Err(e) => warn!("[fwa-agentx] video extract error for {}: {}", name, e),
                    }
                    continue;
                }

                if mime.starts_with("text/")
                    || name_lower.ends_with(".txt")
                    || name_lower.ends_with(".csv")
                    || name_lower.ends_with(".json")
                {
                    let text = String::from_utf8_lossy(&data).to_string();
                    out.text_files.push((name, text));
                    continue;
                }

                // Unknown — drop with a warning
                warn!(
                    "[fwa-agentx] unrouted file {} mime={} — dropping",
                    name, mime
                );
            }
            _ => {
                // Ignore unknown part kinds
            }
        }
    }

    out
}

fn extract_task_id(question: &str) -> Option<String> {
    // Mirrors Python regex: r'# Task ID\n(\S+)\n'
    let re = regex::Regex::new(r"(?m)^#\s*Task ID\s*\n(\S+)").ok()?;
    re.captures(question)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

// ───────────────────────────────────────────────────────────────────────────
// Image / PDF / Video decode helpers
// ───────────────────────────────────────────────────────────────────────────

fn reencode_image_to_jpeg(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    // Use the image crate to decode any supported format, then re-encode as JPEG.
    // This normalizes inputs (PNG, WebP, RGBA → RGB JPEG) for the OpenAI vision API.
    let img = image::load_from_memory(data)?;
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    let dyn_img = image::DynamicImage::ImageRgb8(rgb);
    dyn_img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Jpeg)?;
    Ok(out)
}

fn extract_pdf_text(data: &[u8]) -> anyhow::Result<String> {
    let text = pdf_extract::extract_text_from_mem(data)?;
    Ok(text)
}

/// Extract up to `max_frames` JPEG frames from an MP4 video by shelling out to ffmpeg.
/// Returns an empty Vec if ffmpeg isn't available — the caller logs a warning.
fn extract_video_frames(data: &[u8], max_frames: usize) -> anyhow::Result<Vec<Vec<u8>>> {
    use std::io::Write;
    use std::process::Command;

    let tmpdir = tempfile::tempdir()?;
    let video_path = tmpdir.path().join("input.mp4");
    let mut f = std::fs::File::create(&video_path)?;
    f.write_all(data)?;
    drop(f);

    let frames_pattern = tmpdir.path().join("frame_%03d.jpg");
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            video_path.to_str().unwrap(),
            "-vf",
            &format!("fps=1,scale=iw*sar:ih,scale='min(1024,iw)':-2,select='lte(n\\,{})'", max_frames),
            "-frames:v",
            &max_frames.to_string(),
            "-q:v",
            "5",
            frames_pattern.to_str().unwrap(),
        ])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            warn!("[fwa-agentx] ffmpeg not available ({}). Video frames skipped.", e);
            return Ok(Vec::new());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("[fwa-agentx] ffmpeg failed: {}", stderr);
        return Ok(Vec::new());
    }

    let mut frames = Vec::new();
    for i in 1..=max_frames {
        let path = tmpdir.path().join(format!("frame_{:03}.jpg", i));
        if path.exists() {
            if let Ok(bytes) = std::fs::read(&path) {
                frames.push(bytes);
            }
        } else {
            break;
        }
    }
    Ok(frames)
}

// ───────────────────────────────────────────────────────────────────────────
// OpenAI vision call
// ───────────────────────────────────────────────────────────────────────────

async fn call_openai_vision(
    api_key: &str,
    model_priority: &[String],
    min_completion_tokens: u64,
    parts: &FwaParts,
    task_hint: TaskHint,
) -> anyhow::Result<(String, String)> {
    if api_key.is_empty() {
        anyhow::bail!("OPENAI_API_KEY not set");
    }

    let user_content = build_user_content(parts);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;

    let (effort, addendum) = match task_hint {
        TaskHint::JsonReport => ("high",   JSON_REPORT_ADDENDUM),
        TaskHint::JsonArray  => ("high",   JSON_ARRAY_ADDENDUM),
        TaskHint::Numerical  => ("high",   NUMERICAL_ADDENDUM),
        TaskHint::General    => ("medium", ""),
    };

    let system_prompt: String = if addendum.is_empty() {
        FWA_SYSTEM_PROMPT.to_string()
    } else {
        format!("{}\n{}", FWA_SYSTEM_PROMPT, addendum)
    };

    info!("[fwa-agentx] task_hint={:?} reasoning_effort={}", task_hint, effort);

    for model in model_priority {
        let body = json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_content}
            ],
            "max_completion_tokens": MAX_COMPLETION_TOKENS,
            "reasoning_effort": effort
        });

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;

            let resp_result = client
                .post(OPENAI_DIRECT_ENDPOINT)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            let resp = match resp_result {
                Ok(r) => r,
                Err(e) => {
                    if attempt < RETRY_MAX_ATTEMPTS {
                        let delay = backoff_delay(attempt);
                        warn!(
                            "[fwa-agentx] {} connection error attempt={} retry in {}ms: {}",
                            model, attempt, delay, e
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        continue;
                    }
                    warn!(
                        "[fwa-agentx] {} connection error after {} attempts, next model: {}",
                        model, attempt, e
                    );
                    break;
                }
            };

            let status = resp.status();

            if status.as_u16() == 429 {
                let retry_after_ms = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s.saturating_mul(1000))
                    .unwrap_or_else(|| backoff_delay(attempt));

                if attempt < RETRY_MAX_ATTEMPTS {
                    warn!(
                        "[fwa-agentx] {} 429 attempt={} retry in {}ms",
                        model, attempt, retry_after_ms
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(retry_after_ms.min(30_000))).await;
                    continue;
                }
                warn!("[fwa-agentx] {} 429 after {} attempts, next model", model, attempt);
                break;
            }

            if status.is_server_error() {
                if attempt < RETRY_MAX_ATTEMPTS {
                    let delay = backoff_delay(attempt);
                    warn!(
                        "[fwa-agentx] {} {} attempt={} retry in {}ms",
                        model, status, attempt, delay
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }
                warn!(
                    "[fwa-agentx] {} {} after {} attempts, next model",
                    model, status, attempt
                );
                break;
            }

            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                warn!(
                    "[fwa-agentx] {} HTTP error {}: {}",
                    model,
                    status,
                    safe_slice(&text, 300)
                );
                break;
            }

            let data: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!("[fwa-agentx] {} response JSON parse error: {}", model, e);
                    break;
                }
            };

            let finish_reason = data["choices"][0]["finish_reason"]
                .as_str()
                .unwrap_or("missing")
                .to_string();

            let usage = &data["usage"];
            let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
            let completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);
            let reasoning_tokens = usage["completion_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0);

            let text = data["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();

            if text.is_empty() {
                warn!(
                    "[fwa-agentx] {} EMPTY: finish={} prompt={} completion={} reasoning={}",
                    model, finish_reason, prompt_tokens, completion_tokens, reasoning_tokens
                );
                break;
            }

            if finish_reason == "length" {
                warn!(
                    "[fwa-agentx] {} TRUNCATED: prompt={} completion={} reasoning={}",
                    model, prompt_tokens, completion_tokens, reasoning_tokens
                );
                // Truncated — but we still have partial output. For FWA we'd rather
                // return what we have than fall through, since the green agent's
                // fuzzy_match may still accept it.
                let cleaned = clean_json_response(&text);
                return Ok((cleaned, model.to_string()));
            }

            if completion_tokens > 0 && completion_tokens < min_completion_tokens {
                warn!(
                    "[fwa-agentx] {} SHORT: completion={} below gate={}",
                    model, completion_tokens, min_completion_tokens
                );
                break;
            }

            info!(
                "[fwa-agentx] {} OK: prompt={} completion={} reasoning={} answer={:?}",
                model,
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                safe_slice(&text, 150)
            );

            let cleaned = clean_json_response(&text);
            return Ok((cleaned, model.to_string()));
        }
    }

    anyhow::bail!("All models failed (empty, truncated, short, rate-limited, or HTTP error)")
}

/// Build the OpenAI Chat Completions `user` message content as an array of
/// text + image_url parts, matching the vision API spec.
fn build_user_content(parts: &FwaParts) -> Value {
    let mut content: Vec<Value> = Vec::new();

    // Lead with the task question (most important)
    if !parts.question.is_empty() {
        content.push(json!({"type": "text", "text": parts.question}));
    }

    // Inline text-file contents (bbox files, plain text, CSVs)
    for (name, text) in &parts.bbox_texts {
        content.push(json!({
            "type": "text",
            "text": format!("Content of {}:\n\n{}", name, text)
        }));
    }
    for (name, text) in &parts.text_files {
        content.push(json!({
            "type": "text",
            "text": format!("Content of {}:\n\n{}", name, text)
        }));
    }
    for (name, text) in &parts.pdf_texts {
        content.push(json!({
            "type": "text",
            "text": format!("Content of {} (extracted text):\n\n{}", name, text)
        }));
    }

    // Images
    for (name, jpeg_bytes) in &parts.images {
        let b64 = b64_encode(jpeg_bytes);
        let data_uri = format!("data:image/jpeg;base64,{}", b64);
        content.push(json!({
            "type": "text",
            "text": format!("Image: {}", name)
        }));
        content.push(json!({
            "type": "image_url",
            "image_url": {"url": data_uri, "detail": "high"}
        }));
    }

    // Video frames (already JPEG)
    if !parts.video_frames.is_empty() {
        content.push(json!({
            "type": "text",
            "text": format!("Video: {} frames extracted at 1-second intervals.", parts.video_frames.len())
        }));
        for (name, jpeg_bytes) in &parts.video_frames {
            let b64 = b64_encode(jpeg_bytes);
            let data_uri = format!("data:image/jpeg;base64,{}", b64);
            content.push(json!({
                "type": "text",
                "text": format!("Frame: {}", name)
            }));
            content.push(json!({
                "type": "image_url",
                "image_url": {"url": data_uri, "detail": "low"}
            }));
        }
    }

    // If somehow we have NO content (no question, no files), give the model
    // a stub so the API doesn't 400.
    if content.is_empty() {
        content.push(json!({"type": "text", "text": "(empty task)"}));
    }

    Value::Array(content)
}

// ───────────────────────────────────────────────────────────────────────────
// Output cleaning
// ───────────────────────────────────────────────────────────────────────────

fn clean_json_response(text: &str) -> String {
    // Strip leading/trailing whitespace, fence markers, etc.
    let mut s = text.trim().to_string();

    // Strip leading ```json or ``` and trailing ```
    if let Ok(re) = regex::Regex::new(r"(?s)^```(?:json|JSON)?\s*\n?(.*?)\n?```\s*$") {
        if let Some(cap) = re.captures(&s) {
            if let Some(inner) = cap.get(1) {
                s = inner.as_str().trim().to_string();
            }
        }
    }

    // If it looks like JSON, try to roundtrip it through serde for canonicalization
    let trimmed = s.trim();
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return serde_json::to_string(&v).unwrap_or(s);
        }
    }

    s
}

// ───────────────────────────────────────────────────────────────────────────
// SSE helpers
// ───────────────────────────────────────────────────────────────────────────

fn sse_event_bytes(data: Value) -> bytes::Bytes {
    let json_str = serde_json::to_string(&data).unwrap_or_default();
    bytes::Bytes::from(format!("data: {}\n\n", json_str))
}

// ───────────────────────────────────────────────────────────────────────────
// Small helpers
// ───────────────────────────────────────────────────────────────────────────

fn backoff_delay(attempt: u32) -> u64 {
    let exp = (attempt.saturating_sub(1)).min(4);
    let base = RETRY_BASE_DELAY_MS * (1u64 << exp);
    let jitter = (attempt as u64 * 137) % (base / 5 + 1);
    base + jitter
}

fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD.decode(s)
}

fn safe_slice(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        // Find the nearest char boundary
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}
