// src/forward.rs
// Forwards the verbatim incoming request body to the chosen upstream and
// returns the response in the right shape (SSE stream vs JSON body).
//
// Why verbatim: the specialists already know their own A2A contract.
// The router does not parse, mutate, or re-encode the payload — it
// inspects (read-only) to choose a destination, then forwards bytes
// untouched. This keeps the surface area small and protects the
// specialists' existing leaderboard-tested behavior.

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;
use std::time::Duration;
use tracing::{info, warn};

use crate::probe::{ResponseShape, Upstream};

const FORWARD_TIMEOUT: Duration = Duration::from_secs(900);

pub async fn forward(
    client: &reqwest::Client,
    upstream: &Upstream,
    body: Bytes,
    probe_name: &'static str,
) -> Response {
    info!(
        "[router] forwarding to upstream={} probe={} shape={:?}",
        upstream.url, probe_name, upstream.response_shape
    );

    let req = client
        .post(&upstream.url)
        .header("content-type", "application/json")
        .body(body)
        .timeout(FORWARD_TIMEOUT);

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "[router] upstream send failed probe={} url={} err={}",
                probe_name, upstream.url, e
            );
            return error_json(
                StatusCode::BAD_GATEWAY,
                format!("Upstream {} unreachable: {}", probe_name, e),
            );
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        warn!(
            "[router] upstream non-2xx probe={} status={} body_preview={}",
            probe_name,
            status,
            &text[..text.len().min(200)]
        );
        return error_json(
            StatusCode::BAD_GATEWAY,
            format!("Upstream {} returned {}: {}", probe_name, status, text),
        );
    }

    match upstream.response_shape {
        ResponseShape::Sse => forward_sse(resp, probe_name),
        ResponseShape::Json => forward_json(resp, probe_name).await,
        ResponseShape::JsonAsSse => forward_json_as_sse(resp, probe_name).await,
    }
}

fn forward_sse(resp: reqwest::Response, probe_name: &'static str) -> Response {
    // Stream upstream bytes back to the client as text/event-stream.
    // No buffering — the specialist (e.g. CyberGym's iterative PoC loop)
    // emits status updates over the life of the task; we must not delay them.
    let stream = resp.bytes_stream().map(move |chunk| {
        chunk.map_err(|e| {
            warn!(
                "[router] sse forward chunk error probe={} err={}",
                probe_name, e
            );
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })
    });

    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    headers.insert("x-accel-buffering", "no".parse().unwrap());
    headers.insert("connection", "keep-alive".parse().unwrap());
    headers.insert("x-router-probe", probe_name.parse().unwrap());

    (StatusCode::OK, headers, Body::from_stream(stream)).into_response()
}

async fn forward_json(resp: reqwest::Response, probe_name: &'static str) -> Response {
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!("[router] json forward body read failed probe={} err={}", probe_name, e);
            return error_json(
                StatusCode::BAD_GATEWAY,
                format!("Upstream {} body read failed: {}", probe_name, e),
            );
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("x-router-probe", probe_name.parse().unwrap());
    headers.insert("connection", "close".parse().unwrap());

    (StatusCode::OK, headers, body).into_response()
}

async fn forward_json_as_sse(resp: reqwest::Response, probe_name: &'static str) -> Response {
    // OSWorld green calls send_message_streaming and expects
    // text/event-stream. The agentx-osworld backend returns a single
    // JSON-RPC object whose result is a completed Task — a valid
    // terminal event. Wrap that JSON as one SSE frame.
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!("[router] json-as-sse body read failed probe={} err={}", probe_name, e);
            return error_json(
                StatusCode::BAD_GATEWAY,
                format!("Upstream {} body read failed: {}", probe_name, e),
            );
        }
    };
    let json_text = String::from_utf8_lossy(&body);
    let frame = format!("data: {}\n\n", json_text.trim());

    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    headers.insert("x-accel-buffering", "no".parse().unwrap());
    headers.insert("x-router-probe", probe_name.parse().unwrap());

    (StatusCode::OK, headers, frame).into_response()
}

pub fn error_json(status: StatusCode, msg: String) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": -32603,
            "message": msg
        }
    });
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    (
        status,
        headers,
        serde_json::to_vec(&body).unwrap_or_default(),
    )
        .into_response()
}
