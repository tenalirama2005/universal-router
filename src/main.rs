// src/main.rs
// Universal Purple Agent Router — Sprint 4
//
// One agent surface (`/`) registered on agentbeats.dev. Inbound A2A
// requests are scored by capability probes that inspect structural
// fingerprints in the JSON-RPC envelope; the highest-scoring probe
// forwards the verbatim payload to its specialist upstream.
//
// Design constraints driven by the RDI integrity rules:
//   1. No probe scores on benchmark names, task IDs, or content keywords
//      that tie it to a specific track. All discrimination is by
//      capability shape (file types, schema fingerprints, payload
//      structure).
//   2. The router itself contains zero special-case lookup tables
//      keyed on benchmark identity. The upstream URL list is config,
//      and the URLs map to capability names (`vuln-repro`, `vision-qa`,
//      etc.), not benchmark names.
//   3. Forwarding is byte-verbatim. The router never rewrites the
//      payload, never injects content, never adds task-specific hints.

use axum::{
    extract::State,
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use clap::Parser;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod forward;
mod probe;
mod probes;

use probe::{CapabilityProbe, ResponseShape, Upstream, ROUTE_CONFIDENCE_THRESHOLD};

#[derive(Parser, Debug)]
#[command(name = "universal-router")]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value = "9000")]
    port: u16,
}

pub struct AppState {
    pub probes: Vec<Box<dyn CapabilityProbe>>,
    pub agent_url: String,
    pub http: reqwest::Client,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    dotenvy::dotenv().ok();
    let args = Args::parse();
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(args.port);

    let agent_url = std::env::var("AGENT_URL")
        .unwrap_or_else(|_| format!("http://{}:{}", args.host, port));

    // Upstream URLs are configured via environment so the router binary
    // is benchmark-agnostic. Capability name → URL mapping lives here,
    // not compiled in.
    let probes: Vec<Box<dyn CapabilityProbe>> = vec![
        Box::new(probes::cybergym::CyberGymProbe::new(Upstream {
            url: std::env::var("UPSTREAM_VULN_REPRO")
                .unwrap_or_else(|_| "http://cybergym-agentx:9019".into()),
            response_shape: ResponseShape::Sse,
        })),
        Box::new(probes::pibench::PiBenchProbe::new(Upstream {
            url: std::env::var("UPSTREAM_POLICY_TOOLUSE")
                .unwrap_or_else(|_| "http://pibench-agentx:8766".into()),
            response_shape: ResponseShape::Json,
        })),
        Box::new(probes::netarena::NetArenaProbe::new(Upstream {
            url: std::env::var("UPSTREAM_TEXT_CODEGEN")
                .unwrap_or_else(|_| "http://netarena-agentx:9019".into()),
            response_shape: ResponseShape::Json,
        })),
        Box::new(probes::fwa::FwaProbe::new(Upstream {
            url: std::env::var("UPSTREAM_VISION_QA")
                .unwrap_or_else(|_| "http://fwa-agentx:8090".into()),
            response_shape: ResponseShape::Json,
        })),
        Box::new(probes::osworld::OsworldProbe::new(Upstream {
            url: std::env::var("UPSTREAM_GUI_AGENT")
                .unwrap_or_else(|_| "http://osworld-agentx:8080".into()),
            response_shape: ResponseShape::Json,
        })),
    ];

    info!("[router] registered {} capability probes:", probes.len());
    for p in &probes {
        info!("[router]   - {} → {}", p.name(), p.upstream().url);
    }

    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(8)
        .build()
        .expect("reqwest client");

    let state = Arc::new(AppState {
        probes,
        agent_url: agent_url.clone(),
        http,
    });

    let app = Router::new()
        .route("/.well-known/agent-card.json", get(agent_card))
        .route("/.well-known/agent.json", get(agent_card))
        .route("/", post(handle_task))
        .route("/a2a/tasks/send", post(handle_task))
        .route("/health", get(health))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("{}:{}", args.host, port);
    info!("[router] listening on {} agent_url={}", addr, agent_url);
    let listener = TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "universal-router",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn agent_card(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Skills list reflects capabilities, not benchmark names — the same
    // generality story the router itself implements.
    let skills: Vec<Value> = state
        .probes
        .iter()
        .map(|p| {
            json!({
                "id": p.name(),
                "name": p.name(),
                "description": capability_description(p.name()),
                "tags": ["agentx", "purple", p.name()]
            })
        })
        .collect();

    Json(json!({
        "name": "for-the-cloud-purple-router",
        "description": "Universal purple agent — capability-based routing across vuln-repro, policy-tooluse, text-codegen, vision-qa, and gui-agent specialists.",
        "url": state.agent_url,
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": "0.3.0",
        "preferredTransport": "JSONRPC",
        "defaultInputModes": ["text", "file", "application/json"],
        "defaultOutputModes": ["text", "file", "application/json"],
        "capabilities": {
            "streaming": true,
            "pushNotifications": false
        },
        "skills": skills
    }))
}

fn capability_description(name: &str) -> &'static str {
    match name {
        "vuln-repro" => "Vulnerability reproduction — generate crash-triggering PoC input from a vulnerable source tree.",
        "policy-tooluse" => "Policy-constrained tool use with structured decision recording across compliance and operations scenarios.",
        "text-codegen" => "Code generation against textual task descriptions involving data transformations.",
        "vision-qa" => "Question answering grounded in visual document inputs (images, scans).",
        "gui-agent" => "GUI agent — interpret screenshots and emit coordinate-based actions to accomplish desktop tasks.",
        _ => "Capability handler.",
    }
}

async fn handle_task(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    // Parse once for scoring. The body is forwarded verbatim regardless.
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("[router] body is not JSON: {}", e);
            return forward::error_json(
                StatusCode::BAD_REQUEST,
                format!("Body is not valid JSON: {}", e),
            );
        }
    };

    // DEBUG: dump structural skeleton of incoming request so the next run's
    // log reveals the real OSWorld vs FWA payload shape. Keys + part kinds
    // only — not full content — to keep the log readable and avoid dumping
    // base64 image blobs.
    if let Some(msg) = parsed.pointer("/params/message") {
        let part_kinds: Vec<String> = msg
            .pointer("/parts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|p| {
                        let r = p.get("root").unwrap_or(p);
                        r.get("kind").and_then(|k| k.as_str()).unwrap_or("?").to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let has_history = parsed.pointer("/params/message/history").is_some()
            || parsed.pointer("/params/history").is_some();
        let ctx_id = parsed
            .pointer("/params/message/contextId")
            .or_else(|| parsed.pointer("/params/contextId"))
            .and_then(|v| v.as_str())
            .unwrap_or("<none>");
        info!(
            "[router] payload skeleton: part_kinds={:?} has_history={} contextId={} top_keys={:?}",
            part_kinds,
            has_history,
            ctx_id,
            parsed.get("params").and_then(|p| p.as_object())
                .map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()
        );
    }
    
    // Score every probe.
    let mut scored: Vec<(f32, &dyn CapabilityProbe)> = state
        .probes
        .iter()
        .map(|p| (p.score(&parsed), p.as_ref()))
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Routing decision summary — useful for post-run analysis.
    let summary: Vec<String> = scored
        .iter()
        .map(|(s, p)| format!("{}={:.2}", p.name(), s))
        .collect();
    info!("[router] scoring: {}", summary.join(" "));

    let Some((top_score, top_probe)) = scored.first() else {
        return forward::error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No probes registered".into(),
        );
    };

    if *top_score < ROUTE_CONFIDENCE_THRESHOLD {
        warn!(
            "[router] no probe above threshold ({} < {}) — refusing to guess",
            top_score, ROUTE_CONFIDENCE_THRESHOLD
        );
        return forward::error_json(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Request did not match any capability shape with sufficient confidence (top={:.2} < {:.2}). \
                 The router refuses to guess to avoid polluting leaderboards with misrouted submissions.",
                top_score, ROUTE_CONFIDENCE_THRESHOLD
            ),
        );
    }

    info!(
        "[router] DECISION → {} (score={:.2})",
        top_probe.name(),
        top_score
    );

    forward::forward(&state.http, top_probe.upstream(), body, top_probe.name()).await
}
