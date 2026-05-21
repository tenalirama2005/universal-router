# Universal Purple Agent Router

Sprint 4 submission for AgentX-AgentBeats Phase 2. One purple agent surface,
five capability backends, capability-shape dispatch.

## Architecture

```
                          ┌────────────────────────┐
   AgentBeats green ───► HTTPS                       │
   agents (any track)    │  Istio Gateway           │
                         │  (TLS termination)        │
                         └──────────┬───────────────┘
                                    │ HTTP, mTLS via Istio sidecars
                                    ▼
                         ┌────────────────────────┐
                         │  Router (replicas: 2)   │
                         │  ─ scores 5 probes      │
                         │  ─ forwards verbatim    │
                         └──────────┬───────────────┘
                                    │
                ┌───────────┬───────┼───────┬───────────┐
                ▼           ▼       ▼       ▼           ▼
            ┌───────┐  ┌───────┐ ┌─────┐ ┌─────┐ ┌─────────┐
            │ vuln- │  │policy-│ │text-│ │vis- │ │ gui-    │
            │ repro │  │tooluse│ │codeg│ │ion- │ │ agent   │
            │ (Cyber│  │(Pi-   │ │(Net-│ │qa   │ │ (OS-    │
            │  Gym) │  │ Bench)│ │Arena│ │(FWA)│ │  World) │
            └───────┘  └───────┘ └─────┘ └─────┘ └─────────┘
                       ClusterIP services only — not externally reachable.
```

## Routing decision: structural probe scoring

The router does not match on benchmark names, task IDs, or content keywords.
Each `CapabilityProbe` scores an incoming A2A request against the *shape* of
work its backend handles — file types, schema fingerprints, payload
structure — and the router forwards to the argmax probe above a confidence
threshold (0.35).

Probes and their structural signals:

| Capability         | Signal                                                              | Backend     |
|--------------------|---------------------------------------------------------------------|-------------|
| `vuln-repro`       | `parts[].file.name` ends `.tar.gz`/`.patch`/`.diff`, or `data.exit_code` present | CyberGym    |
| `policy-tooluse`   | `parts[0].data.bootstrap=true` OR `data.{context_id,messages}` + OpenAI tools schema | Pi-Bench    |
| `text-codegen`     | Text-only envelope with no other capability-specific signals (weak default) | NetArena    |
| `vision-qa`        | `file.mimeType=image/*` + text part                                 | FWA         |
| `gui-agent`        | Image + data part with `action_space`/`observation` schema          | OSWorld     |

If no probe scores above threshold, the router returns a 422 with an error
rather than guessing — a misroute is worse than a refusal because it would
pollute the leaderboard with a nonsense submission.

## Compliance with Phase 2 Sprint 4 rules

The Phase 2 Sprint 4 brief requires that purple agents demonstrate
generality "without benchmark-specific hardcoding or special-case lookup
tables." This implementation satisfies that by:

1. **Probes score on structural shape, not benchmark identity.** No probe
   matches on the strings `"FWA"`, `"OSWorld"`, `"Pi-Bench"`, ARVO task IDs,
   or any benchmark-name keyword. Signals are JSON Schema fingerprints,
   MIME types, and field-presence checks.

2. **Capability names, not benchmark names.** The router's agent-card lists
   skills as `vuln-repro`, `policy-tooluse`, `text-codegen`, `vision-qa`,
   `gui-agent` — verbs describing work shape. A new benchmark that fits
   one of these shapes routes correctly without code changes.

3. **Upstream URL mapping lives in config, not code.** Adding a sixth
   capability is: add a probe file, add a ConfigMap key, add a Service.
   No router binary change required to support it.

## Layout

```
.
├── Cargo.toml
├── Dockerfile                        # multi-stage musl → distroless
├── src/
│   ├── main.rs                       # axum, scoring loop, agent-card
│   ├── probe.rs                      # CapabilityProbe trait
│   ├── forward.rs                    # SSE + JSON verbatim forwarding
│   └── probes/
│       ├── cybergym.rs
│       ├── pibench.rs
│       ├── netarena.rs
│       ├── fwa.rs
│       └── osworld.rs
└── k8s/
    └── base/
        ├── 00-namespace-and-config.yaml
        ├── 05-secret-template.yaml   # documented imperative create
        ├── 10-router.yaml            # Deployment + Service, 2 replicas
        ├── 20-specialists.yaml       # 5 Deployments + 5 ClusterIP Services
        ├── 30-network-policy.yaml    # default-deny + explicit allows
        ├── 40-istio-gateway.yaml     # Gateway + VirtualService + STRICT mTLS
        └── kustomization.yaml
```

## Deployment

```bash
# 1. Build and push the router image.
docker build -t tenalirama2026/agentx-purple-router:latest .
docker push tenalirama2026/agentx-purple-router:latest

# 2. Create the Azure OpenAI Secret (never committed).
kubectl create secret generic azure-openai-creds \
  --namespace agentx-purple \
  --from-literal=AZURE_OPENAI_KEY="$AZURE_OPENAI_KEY" \
  --from-literal=AZURE_OPENAI_ENDPOINT="$AZURE_OPENAI_ENDPOINT"

# 3. Apply everything.
kubectl apply -k k8s/base/

# 4. Verify.
kubectl -n agentx-purple get pods,svc,virtualservice
kubectl -n agentx-purple logs deploy/router -f
```

## Verifying isolation

Specialists must not be reachable externally. From an off-cluster machine:

```bash
# Should TIMEOUT or 503 — there is no external route to the specialists.
curl -m 5 https://agentx.forthecloudbythecloud.in/cybergym/health || echo "OK: blocked"

# Should succeed — the router is the only exposed surface.
curl https://agentx.forthecloudbythecloud.in/.well-known/agent-card.json
```

From inside the cluster (debug pod), specialists are reachable by ClusterIP DNS:

```bash
kubectl -n agentx-purple run debug --rm -it --image=curlimages/curl -- sh
# inside:
curl http://cybergym-specialist:9019/health
curl http://pibench-specialist:8766/health
```

## Logs and observability

Every routing decision is logged at `info` level on the router:

```
[router] scoring: vuln-repro=0.95 policy-tooluse=0.00 text-codegen=0.00 vision-qa=0.00 gui-agent=0.00
[router] DECISION → vuln-repro (score=0.95)
[router] forwarding to upstream=http://cybergym-specialist:9019 probe=vuln-repro shape=Sse
```

Istio sidecars add automatic per-route latency, error rate, and request
count metrics to Prometheus — useful for the cost-efficiency dimension of
judging.
