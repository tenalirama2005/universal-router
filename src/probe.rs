// src/probe.rs
// CapabilityProbe trait — the core routing abstraction.
//
// Each probe scores an incoming A2A request against the SHAPE of work
// its specialist can handle. Scoring is structural — field presence,
// nested value shapes, schema fingerprints — NOT benchmark name matching
// or task-id keyword detection. This keeps the router on the right side
// of the RDI rule against "benchmark-specific hardcoding or special-case
// lookup tables."
//
// The router calls score() on every probe, picks argmax above a
// confidence threshold, and forwards the request body verbatim to that
// probe's upstream. Below threshold → A2A-shaped error.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseShape {
    /// Returns a Server-Sent Events stream (text/event-stream).
    Sse,
    /// Returns a single JSON body.
    Json,
}

#[derive(Debug, Clone)]
pub struct Upstream {
    pub url: String,
    pub response_shape: ResponseShape,
}

pub trait CapabilityProbe: Send + Sync {
    /// Human-readable identifier — used only for logging and the routing
    /// decision header, never for matching logic.
    fn name(&self) -> &'static str;

    /// Score in [0.0, 1.0] indicating how well `req` matches this
    /// probe's capability shape. Scoring MUST be based on structural
    /// signals from the JSON-RPC envelope and `params.message` payload —
    /// presence of fields, shape of values, schema fingerprints — never
    /// on benchmark names, task ID strings, or content keyword matching
    /// that ties the probe to a specific benchmark.
    fn score(&self, req: &Value) -> f32;

    /// Upstream specialist endpoint and the response shape it returns.
    fn upstream(&self) -> &Upstream;
}

/// Minimum score required to route. Below this, the router returns an
/// A2A error rather than guessing. Tuned conservatively — false routes
/// are worse than refusals because they pollute leaderboards with
/// nonsense submissions.
pub const ROUTE_CONFIDENCE_THRESHOLD: f32 = 0.35;
