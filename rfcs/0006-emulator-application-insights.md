---
rfc: 0006
title: "Application Insights Telemetry for DocumentDB Local Emulator"
status: Draft
owner: "@Ritvik-Jayaswal"
issue: "https://github.com/documentdb/documentdb/issues/XXX"
discussion: "https://github.com/documentdb/documentdb/discussions/XXX"
version-target: 1.0
implementations:
  - "https://github.com/documentdb/documentdb/pull/XXX"
---

# RFC-0006: Application Insights Telemetry for DocumentDB Local Emulator

## Problem

The DocumentDB local emulator lacks structured, actionable telemetry. Without it, the team has no visibility into how users interact with the emulator: which features they exercise, what kinds of queries they run, where they encounter errors, and whether the emulator is being used at all or abandoned quickly.

### Who Is Impacted

- **Product and engineering teams** — cannot make data-driven decisions about which features to prioritize, which bugs matter most, or where the emulator diverges from customer expectations.
- **Customers** — indirectly affected when compatibility gaps and pain points are invisible to the team and therefore remain unresolved.

### Consequences of Not Solving This

- Investment is allocated to features that aren't being used, while real friction points remain invisible.
- Query incompatibilities or emulator bugs that affect large numbers of users go undetected until customers escalate manually.
- There is no baseline to measure the impact of improvements.

### Current State

The emulator entrypoint already exposes an `--enable-telemetry` flag (and `ENABLE_TELEMETRY` environment variable) in [`scripts/emulator_entrypoint.sh`](../scripts/emulator_entrypoint.sh), but the flag is only validated — it is never forwarded to the gateway process or used to enable any telemetry pipeline. No telemetry data is currently collected or transmitted.

A comparable implementation was completed for the DocumentDB Kubernetes Operator ([PR #237](https://github.com/documentdb/documentdb-kubernetes-operator/pull/237)), which integrated the Microsoft Application Insights Go SDK, wired telemetry into controllers, and exposed Helm chart configuration for the connection string. This RFC adapts that approach for the emulator context.

### Success Criteria

1. When `--enable-telemetry true` is passed (or `ENABLE_TELEMETRY=true` is set), the emulator emits structured events to Application Insights without any observable impact on functional behavior.
2. No personally identifiable information (PII), credentials, collection names, field names, or raw query values are ever transmitted.
3. Query shape telemetry captures the *types* of operations customers run (find, aggregate, insert, update, delete, command) and the pipeline stages or operators involved, without capturing actual values.
4. Users can opt out at any time by omitting the flag or setting `ENABLE_TELEMETRY=false` (which should remain the default).
5. Telemetry is silently disabled if the Application Insights connection string is absent or malformed, with a single log warning — it must never crash the emulator.

### Non-Goals

- Collecting raw query text, filter values, or document contents.
- Collecting usernames, passwords, database names, collection names, or index names.
- Real-time streaming or alerting (Application Insights handles aggregation and alerting on the backend).
- Exposing telemetry data to end users or external systems other than the team's Application Insights workspace.
- Replacing or modifying the existing PostgreSQL/gateway logging subsystem.

---

## Approach

### Proposed Solution

Implement a lightweight telemetry client in the gateway (Rust, [`pg_documentdb_gw`](../pg_documentdb_gw)) that:

1. Reads the opt-in flag and Application Insights connection string at startup.
2. Emits a small set of lifecycle and query-shape events to Application Insights using the [Application Insights REST ingestion API](https://learn.microsoft.com/en-us/azure/azure-monitor/app/api-custom-events-metrics), batched and sent asynchronously so the hot path is never blocked.
3. Hashes or omits all user-identifiable fields before transmission (stable, one-way hash of the container instance to correlate sessions without exposing identity).

The connection string for the team's Application Insights workspace is **baked into the release build** via a CI/CD secret (matching the pattern used in the Kubernetes operator PR), so users never need to supply it. Opt-in/opt-out is purely the `ENABLE_TELEMETRY` flag.

### Key Benefits and Tradeoffs

| Benefit | Tradeoff |
|---|---|
| Actionable usage data with zero configuration burden on users | Requires baking a connection string into the release artifact (mitigated: read-only ingestion key, no data exfiltration risk) |
| Consistent with Application Insights already used across the DocumentDB ecosystem | Adds a new async background task to the gateway process |
| Opt-in by default protects privacy and builds user trust | Opt-in means lower data volume initially; consider defaulting to opt-in with clear disclosure in the future |
| Query shape data (operators used, pipeline stages) reveals compatibility gaps | Requires careful scrubbing logic to ensure no value leakage |

### Alignment with Existing Architecture

The gateway process already manages the full lifecycle of the emulator's MongoDB wire protocol layer. Adding telemetry as a background task within the gateway keeps all observable behavior centralized, avoids a separate sidecar process, and reuses the existing `SetupConfiguration.json` config file for any gateway-level telemetry settings.

---

## Detailed Design

### Technical Details

#### Telemetry Client

A new Rust module `documentdb_gateway_core::telemetry` will own all telemetry logic:

- **Initialization:** On startup, if `ENABLE_TELEMETRY=true`, parse the baked-in (compile-time) Application Insights connection string from an env var injected at build time (`APPINSIGHTS_CONNECTION_STRING`). If absent or malformed, log a single warning and disable telemetry for the process lifetime.
- **Async sender:** Spawn a Tokio background task that owns an in-memory queue. The hot path (request handling) posts events to the queue without awaiting. The background task batches up to 100 events or flushes every 30 seconds, matching the Kubernetes operator's batching parameters.
- **Graceful shutdown:** On `SIGTERM`/`SIGINT`, flush any queued events before the gateway exits (best-effort, timeout 5 seconds).
- **Cloud/environment detection:** Detect whether the emulator is running inside Docker, a devcontainer, GitHub Codespaces, CI (via common env vars like `CI`, `CODESPACES`, `GITHUB_ACTIONS`), or bare-metal, and include this as an anonymized `environment` property on every event.

#### Session Correlation

At startup, generate a random UUID (`session_id`) for the process lifetime. This correlates events within a single emulator run without identifying the user across runs. Never persist this value to disk.

#### PII Scrubbing

All event properties must pass through a scrubbing layer before being enqueued:

- **Never included:** usernames, passwords, database names, collection names, field names, index names, hostnames, IP addresses, file paths.
- **Hashed (one-way SHA-256, truncated to 8 bytes, hex-encoded):** The `session_id` only. No other identifier is hashed and transmitted.
- **Included as-is:** emulator version, OS family (Linux/Windows/macOS — not version string), environment tag, operation type enum, pipeline stage names (these are part of the MongoDB specification, not user data), error code integers, duration buckets.

#### Query Shape Telemetry

Wire protocol command handlers will emit a `QueryExecuted` event (non-blocking enqueue) containing:

```
{
  "name": "QueryExecuted",
  "properties": {
    "session_id": "<hex>",
    "emulator_version": "1.2.3",
    "operation": "find" | "aggregate" | "insert" | "update" | "delete" | "findAndModify" | "count" | "distinct" | "command",
    "pipeline_stages": ["$match", "$group", "$sort"],   // aggregate only; stage names only, no values
    "has_filter": true | false,
    "has_projection": true | false,
    "has_sort": true | false,
    "has_limit": true | false,
    "has_skip": true | false,
    "index_used": true | false,
    "duration_bucket_ms": "0-1" | "1-10" | "10-100" | "100-1000" | "1000+",
    "success": true | false,
    "error_code": 2,                                    // only when success=false; integer wire protocol code
    "environment": "docker" | "codespaces" | "ci" | "bare-metal" | "unknown"
  }
}
```

#### Lifecycle Events

| Event | When Emitted | Key Properties |
|---|---|---|
| `EmulatorStarted` | Gateway ready to accept connections | `emulator_version`, `session_id`, `environment`, `tls_enabled`, `custom_port` (bool), `extended_rum_enabled` |
| `EmulatorStopped` | Graceful shutdown complete | `session_id`, `uptime_bucket_minutes` |
| `InitDataLoaded` | Sample or custom init data loaded | `session_id`, `data_source` (`sample` / `custom`) |
| `QueryExecuted` | Each wire protocol command completes | (see above) |

### API Changes

No public-facing API changes. The `--enable-telemetry` flag and `ENABLE_TELEMETRY` environment variable already exist in the entrypoint; this RFC gives them a real implementation.

A new optional key `AppInsightsConnectionString` will be recognized in `SetupConfiguration.json` so the gateway can receive the connection string from the entrypoint when needed for local development and testing. In release builds this is provided via build-time injection, not the config file.

### Database Schema Changes

None.

### Configuration Changes

| Setting | Where | Default | Description |
|---|---|---|---|
| `ENABLE_TELEMETRY` | Env var / `--enable-telemetry` CLI flag | `false` | Master opt-in switch. Must be `true` to emit any data. |
| `APPINSIGHTS_CONNECTION_STRING` | Build-time env var (CI secret) | *(injected at release build)* | Application Insights ingestion endpoint + key. Never user-facing. |
| `AppInsightsConnectionString` | `SetupConfiguration.json` (optional) | *(empty)* | Override for local development / testing only. Ignored in release builds when the build-time value is present. |

The entrypoint script ([`scripts/emulator_entrypoint.sh`](../scripts/emulator_entrypoint.sh)) will be updated to forward `ENABLE_TELEMETRY` and (when present) `APPINSIGHTS_CONNECTION_STRING` to the gateway via the `SetupConfiguration.json` config file, mirroring how `GatewayListenPort` and `PostgresPort` are already injected.

### Testing Strategy

- **Unit tests:** PII scrubbing logic (verify field names, values, and paths are not present in the serialized event payload). Query shape extraction (verify no values leak, verify stage names are correctly enumerated). Connection string parsing (whitespace, malformed strings, missing key).
- **Integration tests:** Start the emulator with `ENABLE_TELEMETRY=false` and verify no outbound HTTP traffic to Application Insights endpoints. Start with a mock HTTP server in place of Application Insights and verify event schema matches the spec above.
- **Regression tests:** Telemetry failures (network unavailable, bad connection string) must not crash the emulator or affect query results.
- **PII audit:** Automated test fixture that runs a set of queries containing known sentinel values and asserts those sentinels are absent from every serialized event.

### Migration Path

- No migration required for existing emulator users. The `ENABLE_TELEMETRY` flag defaults to `false`; existing users who do not set it are unaffected.
- Users who already set `ENABLE_TELEMETRY=true` (expecting future functionality) will begin receiving the telemetry pipeline with no additional configuration.
- Backwards compatible: the flag will continue to be validated and silently ignored if telemetry is disabled.

### Documentation Updates

- [`scripts/emulator_entrypoint.sh`](../scripts/emulator_entrypoint.sh): Update the `--enable-telemetry` help text to accurately describe what data is collected and link to the privacy statement.
- `README.md` (emulator): Add a "Telemetry" section describing opt-in behavior, what is and is not collected, and how to disable.
- Release notes: Disclose the telemetry addition clearly in the changelog entry for the first release containing this feature.
- Internal runbook: Document how to query the Application Insights workspace for usage and query shape reports.

---

## Implementation Tracking

*This section SHALL be populated during the Implementation phase.*

### Implementation PRs

- [ ] PR #XXX: Telemetry client module in `documentdb_gateway_core` (batching, PII scrubbing, Application Insights HTTP sender)
- [ ] PR #XXX: Wire `EmulatorStarted` / `EmulatorStopped` lifecycle events into the gateway startup/shutdown path
- [ ] PR #XXX: Wire `QueryExecuted` events into wire protocol command handlers
- [ ] PR #XXX: Forward `ENABLE_TELEMETRY` and connection string from `emulator_entrypoint.sh` to gateway config
- [ ] PR #XXX: CI/CD secret injection of `APPINSIGHTS_CONNECTION_STRING` for release builds
- [ ] PR #XXX: Documentation and README updates

### Status Updates

**2026-03-10:** RFC created and submitted for initial feedback.

### Open Questions

- [ ] **Opt-in vs. opt-out default:** Should telemetry default to `true` (opt-out) with a clear first-run disclosure banner, or remain `false` (opt-in)? Opt-out would yield significantly more data but requires legal/privacy review.
  - Discussion: TBD
- [ ] **Query shape granularity for `command`:** For generic `command` operations (e.g., `listCollections`, `createIndexes`, `dropDatabase`), should we emit the command name as a property (it is part of the spec, not user data) or group all under `"command"`?
  - Discussion: TBD
- [ ] **Duration bucket resolution:** Are five duration buckets (0–1 ms, 1–10 ms, 10–100 ms, 100–1000 ms, 1000+ ms) sufficient, or should we include sub-millisecond and multi-second finer buckets?
  - Discussion: TBD
- [ ] **Windows emulator support:** The emulator currently targets Linux containers. If a native Windows build is added in the future, the environment detection and build-time secret injection strategy will need re-evaluation.
  - Discussion: TBD

### Implementation Notes

*Capture important decisions or learnings during implementation*

- **Decision [2026-03-10]:** Use Application Insights REST ingestion API directly rather than an SDK, to avoid adding a heavy dependency to the Rust gateway. The ingestion endpoint accepts standard HTTP POST with JSON payloads and the connection string provides the endpoint URL and instrumentation key.
  - **Context:** The Kubernetes operator used the Go Application Insights SDK; Rust has no official Microsoft SDK, so a thin HTTP client wrapper is more appropriate.
  - **Alternatives:** `appinsights` crate (community, not officially maintained).
