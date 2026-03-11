---
rfc: 0014
title: "Application Insights Telemetry for DocumentDB Local Emulator"
status: Draft
owner: "@Ritvik-Jayaswal"
issue: "https://github.com/documentdb/documentdb/issues/XXX"
discussion: "https://github.com/documentdb/documentdb/discussions/XXX"
version-target: 1.0
implementations:
  - "https://github.com/documentdb/documentdb/pull/XXX"
---

# RFC-0014: Application Insights Telemetry for DocumentDB Local Emulator

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

---

## Approach

### Proposed Solution

Deploy the [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) as an additional process inside the emulator container, managed entirely by the existing entrypoint script. The collector is configured to:

1. **Receive** metrics and log data from PostgreSQL via the `postgresqlreceiver` and `pg_stat_statements`, and from the gateway via its existing log output.
2. **Process** all data through a scrubbing pipeline (`transformprocessor` / `filterprocessor`) to strip PII before any data leaves the machine.
3. **Export** the cleaned telemetry to Application Insights via the `azuremonitorexporter`.

No changes are made to the gateway or PostgreSQL source code. The only code changes are to the OTel Collector configuration file and the emulator entrypoint script. The Application Insights connection string is **baked into the release build** via a CI/CD secret, so users never need to supply it. Opt-in/opt-out is purely the `ENABLE_TELEMETRY` flag.

### Key Benefits and Tradeoffs

| Benefit | Tradeoff |
|---|---|
| Actionable usage data with zero configuration burden on users | Requires baking a connection string into the release artifact (mitigated: read-only ingestion key, no data exfiltration risk) |
| Consistent with Application Insights already used across the DocumentDB ecosystem | Adds the OTel Collector binary to the emulator container image (~100 MB) |
| Opt-in by default protects privacy and builds user trust | Opt-in means lower data volume initially; consider defaulting to opt-in with clear disclosure in the future |
| Query shape data (operators used, pipeline stages) reveals compatibility gaps | Requires careful scrubbing logic to ensure no value leakage |

### Alignment with Existing Architecture

The emulator entrypoint script ([`scripts/emulator_entrypoint.sh`](../scripts/emulator_entrypoint.sh)) already manages the full lifecycle of the PostgreSQL server and gateway process. Adding the OTel Collector as another managed child process follows the same pattern — start on launch, stop on shutdown — without any changes to the gateway or PostgreSQL source code. PostgreSQL's `pg_stat_statements` extension, which is already available in the emulator, provides the query-level statistics needed for usage telemetry.

---

## Detailed Design

### Technical Details

#### OTel Collector Pipeline

The OpenTelemetry Collector runs as a managed child process of the emulator entrypoint. Its configuration (`otelcol-config.yaml`) defines a three-stage pipeline:

**Receivers**
- `postgresqlreceiver` — connects to the local PostgreSQL instance and collects metrics from `pg_stat_statements` (call counts, total execution time per normalized query fingerprint) and standard `pg_stat_*` tables (connection counts, cache hit ratios, table and index sizes).
- `filelogreceiver` — tails the existing gateway log file for error-level events, extracting error codes and command types using regex operators. No query values are captured.

**Processors**
- `transformprocessor` — normalizes query fingerprints produced by `pg_stat_statements` (which already strips literal values) into a bucketed operation type (`find`, `aggregate`, `insert`, `update`, `delete`, `command`) and extracts pipeline stage names for aggregate queries.
- `filterprocessor` — drops any attribute whose key or value matches a blocklist of known PII patterns (collection names, usernames, hostnames, IP addresses, file paths).
- `resourceprocessor` — attaches static resource attributes: `emulator.version`, `environment` tag, and a per-run `session.id` (random UUID generated by the entrypoint script and passed via env var).
- `batchprocessor` — batches up to 100 data points or flushes every 30 seconds before export.

**Exporter**
- `azuremonitorexporter` — sends processed telemetry to Application Insights. The connection string is provided via the `APPLICATIONINSIGHTS_CONNECTION_STRING` environment variable, injected from a CI/CD secret at release build time and forwarded by the entrypoint script.

#### Session Correlation

The entrypoint script generates a random UUID at startup (`SESSION_ID=$(uuidgen)`) and passes it to the OTel Collector as an environment variable. The `resourceprocessor` attaches it as a resource attribute on every exported data point. This correlates events within a single emulator run without identifying the user across runs. The value is never written to disk.

#### PII Scrubbing

The `filterprocessor` and `transformprocessor` enforce the following rules:

- **Never included:** raw query text, literal values, database names, collection names, field names, index names, usernames, hostnames, IP addresses, file paths.
- **Safe to include:** emulator version, OS family, environment tag (`docker` / `codespaces` / `ci` / `bare-metal` / `unknown`), normalized operation type, pipeline stage names (MongoDB spec names, not user data), error code integers, duration histograms, `pg_stat_statements` query fingerprint hash (integer `queryid`, not text).

`pg_stat_statements` already normalizes queries by replacing literal values with `$1`, `$2`, … placeholders before computing the `queryid` hash, so literal values never enter the OTel pipeline.

#### Metrics and Events Collected

**Query usage metrics** (sourced from `pg_stat_statements`, aggregated per flush interval):

| Metric | Type | Description |
|---|---|---|
| `documentdb.query.calls` | Counter | Number of executions per operation type |
| `documentdb.query.duration` | Histogram | Execution time distribution per operation type |
| `documentdb.query.errors` | Counter | Failed executions per error code |

**Resource metrics** (sourced from `pg_stat_*` tables):

| Metric | Type | Description |
|---|---|---|
| `documentdb.connections.active` | Gauge | Active client connections |
| `documentdb.cache.hit_ratio` | Gauge | Buffer cache hit ratio |

**Lifecycle events** (emitted by the entrypoint script to a log file read by `filelogreceiver`):

| Event | When Emitted | Key Attributes |
|---|---|---|
| `EmulatorStarted` | Gateway ready to accept connections | `emulator_version`, `session_id`, `environment`, `tls_enabled`, `extended_rum_enabled` |
| `EmulatorStopped` | Graceful shutdown signal received | `session_id`, `uptime_seconds` |
| `InitDataLoaded` | Sample or custom init data loaded | `session_id`, `data_source` (`sample` / `custom`) |

### API Changes

No public-facing API changes and no changes to the gateway or PostgreSQL source code. The `--enable-telemetry` flag and `ENABLE_TELEMETRY` environment variable already exist in the entrypoint; this RFC gives them a real implementation by starting and stopping the OTel Collector process.

### Database Schema Changes

None.

### Configuration Changes

| Setting | Where | Default | Description |
|---|---|---|---|
| `ENABLE_TELEMETRY` | Env var / `--enable-telemetry` CLI flag | `false` | Master opt-in switch. Must be `true` to start the OTel Collector. |
| `APPLICATIONINSIGHTS_CONNECTION_STRING` | Build-time env var (CI secret) | *(injected at release build)* | Application Insights ingestion endpoint + key. Never user-facing. |
| `SESSION_ID` | Generated by entrypoint at startup | *(random UUID per run)* | Passed to the OTel Collector as a resource attribute for session correlation. |

The entrypoint script ([`scripts/emulator_entrypoint.sh`](../scripts/emulator_entrypoint.sh)) will be updated to:
1. Generate `SESSION_ID` via `uuidgen` at startup.
2. If `ENABLE_TELEMETRY=true`, start the OTel Collector as a background process with `otelcol-config.yaml` and the above env vars.
3. On shutdown (in the existing `cleanup` function), send `SIGTERM` to the OTel Collector and wait up to 5 seconds for it to flush.

### Testing Strategy

- **OTel Collector config validation:** Use `otelcol validate --config otelcol-config.yaml` in CI to catch configuration errors before they reach users.
- **Integration tests:** Start the emulator with `ENABLE_TELEMETRY=false` and verify the OTel Collector process is not spawned and no outbound HTTP traffic is produced. Start with `ENABLE_TELEMETRY=true` and a mock OTLP receiver in place of Application Insights; verify that exported metric names and attributes match the spec above.
- **PII audit:** Automated test that runs a set of queries containing known sentinel string values and asserts those sentinels are absent from every metric attribute exported by the OTel Collector (inspected via the mock OTLP receiver).
- **Resilience tests:** Verify that if the OTel Collector crashes or the Application Insights endpoint is unreachable, the emulator continues serving queries without error.

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

- [ ] PR #XXX: Add `otelcol-config.yaml` (OTel Collector pipeline: `postgresqlreceiver`, `filelogreceiver`, processors, `azuremonitorexporter`)
- [ ] PR #XXX: Update `scripts/emulator_entrypoint.sh` to generate `SESSION_ID`, start/stop the OTel Collector process, and emit lifecycle log events
- [ ] PR #XXX: CI/CD secret injection of `APPLICATIONINSIGHTS_CONNECTION_STRING` for release builds and inclusion of the OTel Collector binary in the container image
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

- **Decision [2026-03-10]:** Use the OpenTelemetry Collector rather than embedding a telemetry SDK in the gateway, so that zero changes are needed to the gateway or PostgreSQL source code.
  - **Context:** The Kubernetes operator embedded the Go Application Insights SDK directly in the controller binary. For the emulator, the OTel Collector provides equivalent functionality as a standalone process, keeps the gateway source clean, and allows the telemetry pipeline to be updated by changing a YAML configuration file rather than recompiling the gateway.
  - **Alternatives:** Embedding the `opentelemetry` Rust crate or a custom Application Insights HTTP client in the gateway — rejected to avoid gateway source changes.
