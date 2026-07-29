---
rfc: 0016
title: "Scarf Usage Telemetry for DocumentDB"
status: Draft
owner: "@RitvikJayaswal"
issue: "https://github.com/documentdb/documentdb/issues/XXX"
version-target: 1.0
implementations:
  - "https://github.com/documentdb/documentdb/pull/XXX"
---

# RFC-0016: Scarf Usage Telemetry for DocumentDB

## Problem

DocumentDB is open source and distributed primarily as a container image
(`documentdb-local`) plus Linux packages. Today the project has **no reliable
signal of real-world adoption**: how many deployments exist, which versions and
platforms are in use, or whether usage is growing. Maintainers and stakeholders
have explicitly asked for open-source adoption numbers, and there is currently no
mechanism to produce them.

### Who is impacted

- **Maintainers / TSC** cannot quantify adoption to prioritize platforms,
  versions, and features, or to justify continued investment.
- **Contributors** lack data about which areas of the project are actually used.
- **Downstream stakeholders** (e.g., the sponsoring organization) cannot report
  meaningful OSS traction.

### Consequences of not solving it

Decisions about roadmap, platform support, and resourcing are made without
adoption data. Registry pull counts alone are misleading — a CI cache pull, a
mirror, or a re-pull of an unchanged image all look identical to a genuine new
deployment, and container registries (GHCR) expose little usable analytics.

### Current workarounds

- **GHCR pull counts:** coarse, easily inflated by CI, no per-version or
  per-platform breakdown, no geographic or organizational signal.
- **Manual anecdote:** issues, stars, Discord activity — not measurable.

### Success criteria

1. A privacy-respecting way to measure **real running deployments** (not just
   downloads), broken down by version and platform.
2. A way to measure **downloads** of the published artifacts.
3. **Off by default**, opt-in, with a standard opt-out that always wins.
4. **Zero data** about user content, queries, schema names, or credentials.
5. **No cost** to the project or to users.
6. Fully auditable in the open-source tree.

### Non-goals

- This RFC does **not** replace or feed operational observability. Detailed
  per-request metrics for operators remain the job of the existing OpenTelemetry
  (OTLP) pipeline and are out of scope here except where the two intersect.
- This RFC does **not** collect any user data, and explicitly does not aim to
  identify individual users.
- This RFC does **not** introduce a general-purpose analytics framework.

---

## Approach

Adopt [Scarf](https://scarf.sh) as the open-source usage-analytics provider,
using two complementary, independent mechanisms:

1. **Distribution analytics (download counting).** Route the project's published
   container-image pull command through a Scarf Gateway domain. Scarf acts as a
   transparent redirect in front of the existing registry (GHCR): users still
   pull the same image with the same digest, but Scarf records the pull
   (version/tag, platform, coarse geo/organization) before redirecting. This
   requires no code — only a one-line change to the documented `docker pull`
   command — and captures **downloads**.

2. **Runtime usage telemetry (deployment counting).** Add a small, optional,
   privacy-respecting telemetry emitter to the gateway that sends two
   low-frequency, aggregated events to a Scarf Event Collection endpoint: a
   one-time **launch** event and a periodic **aggregated summary**. This
   captures **real running deployments**, which downloads cannot.

### Why Scarf

- Purpose-built for open-source adoption analytics (downloads, running
  deployments, version/platform/geo, organization enrichment).
- **Free** for this use: Scarf does not charge for event ingestion, telemetry
  volume, or download traffic at any volume.
- Transparent redirect model means the container image itself is unchanged
  (same digest, same registry backend); distribution can be re-pointed later
  without changing the user's pull command.
- Honors `DO_NOT_TRACK` and provides a documented, cookie-free model.

### Why two mechanisms

Downloads and deployments answer different questions. A download count is
inflated by CI and mirrors and says nothing about whether the software is
actually run. A launch/heartbeat signal measures real deployments by
version/platform. Together they give an adoption funnel (downloaded → run).

### Key tradeoffs

- **Runtime telemetry is inherently a network call from the user's process.**
  We mitigate this by making it **off by default**, opt-in, aggregated,
  fire-and-forget, and fully documented — following Scarf's own best-practice
  guidance (low-frequency, high-intent events only).
- **A user-facing documentation change** is required for download tracking (the
  published pull command must point at the Scarf domain). This is a
  one-line, reversible change with no code impact.

### Fit with existing architecture

The runtime emitter lives in the gateway (`pg_documentdb_gw`) alongside, but
strictly separate from, the existing OpenTelemetry telemetry. It reuses the
existing `TelemetryConfig` / `SetupConfiguration` configuration pattern and the
existing async (Tokio) runtime. It never participates in the request/response
data path beyond incrementing in-process counters.

---

## Detailed Design

### Two independent telemetry systems (must not be conflated)

DocumentDB will have two separate systems that both deal with "metrics." They
are controlled independently and send to different destinations.

| | Operational metrics (OTLP) | Usage telemetry (Scarf) |
|---|---|---|
| Audience | The **operator** running the instance | The **maintainers** of the project |
| Transport | OpenTelemetry OTLP (gRPC) | Plain HTTPS GET to Scarf endpoint |
| Destination | An endpoint the operator configures (Prometheus, Grafana, App Insights) | Scarf Event Collection endpoint |
| Granularity | Per-request, high-cardinality | Aggregated, low-frequency |
| Toggle | `OTEL_METRICS_ENABLED` (+ `OTEL_*`) | `SCARF_ANALYTICS_ENABLED` |
| Default | Off | Off |

The rest of this section specifies the Scarf usage telemetry.

### Exactly what the gateway collects and sends

When (and only when) usage telemetry is enabled, the gateway sends HTTPS
requests to a Scarf Event Collection endpoint. There are exactly **two** event
types, encoded as URL query parameters (no request body).

#### Event: `emulator_launch` (sent once, shortly after startup)

| Field | Example | Meaning |
|-------|---------|---------|
| `event` | `emulator_launch` | Event type |
| `version` | `0.104.0` | Gateway package version (`CARGO_PKG_VERSION`) |
| `os` | `linux` | Compile-time OS constant |
| `arch` | `x86_64` | Compile-time CPU architecture constant |
| `db_system` | `documentdb` | Constant identifier |

#### Event: `gateway_metrics_summary` (sent periodically; default hourly)

Sent only if there was activity in the interval. All values are **process-wide
aggregate counts for the interval** — never per-request, never per-collection,
never per-user.

| Field | Example | Meaning |
|-------|---------|---------|
| `event` | `gateway_metrics_summary` | Event type |
| `version`, `os`, `arch`, `db_system` | (as above) | Same host attributes |
| `operations` | `42` | Total operations handled in the interval |
| `documents_inserted` | `10` | Documents inserted in the interval |
| `documents_returned` | `55` | Documents returned by reads in the interval |
| `documents_updated` | `7` | Documents updated in the interval |
| `documents_deleted` | `3` | Documents deleted in the interval |

That is the complete list of transmitted fields. There are no hidden fields.

### What is never collected

The emitter never reads, constructs, or transmits:

- **User data** — no document contents, field names, or values.
- **Queries** — no filters, aggregation pipelines, or command arguments.
- **User-chosen names** — no database names, collection names, index names, or
  user names.
- **Credentials** — no passwords, connection strings, tokens, or keys.
- **Network identity in the payload** — the gateway does not put IP addresses in
  events. (As with any HTTPS request, the receiving service observes the
  connection's source address and may derive coarse geo/organization signal from
  it; this is inherent to making a network request, not something the payload
  carries.)

### Privacy hardening: hashing user-defined identifiers everywhere

Independently of Scarf, the gateway's OTLP operational metrics previously
attached **raw** user-defined identifiers (database name, collection name) as
metric attributes. This RFC hashes these at the source so raw user-chosen names
never enter **any** telemetry attribute — protecting operators' own
observability backends as well.

- A helper hashes an identifier to the first 16 hex characters of its SHA-256
  digest (stable and correlatable, non-reversible).
- Internal sentinels (`""` for database-level operations, `"unknown"`
  placeholder) pass through unchanged, since they are not user-defined.
- Non-user-defined attributes (`db.system.name` = `documentdb`, the operation
  type such as `Insert`/`Find`) are left intact.

The Scarf usage summary never includes collection/namespace at all — hashed or
otherwise — because it is purely aggregate.

### Configuration

Resolution order for each setting: JSON config (`TelemetryOptions.Scarf`) >
environment variable > built-in default. User opt-out overrides everything.

| Setting | Env var | JSON field | Default |
|---------|---------|-----------|---------|
| Enable | `SCARF_ANALYTICS_ENABLED` | `Enabled` | `false` |
| Endpoint | `SCARF_TELEMETRY_ENDPOINT` | `Endpoint` | `https://documentdb.gateway.scarf.sh/telemetry` |
| Summary interval (ms) | `SCARF_SUMMARY_INTERVAL_MS` | `SummaryIntervalMs` | `3600000` (1 hour) |
| Opt out | `DO_NOT_TRACK=1` **or** `SCARF_NO_ANALYTICS=1` | — | not set |

The default endpoint (`documentdb.gateway.scarf.sh`) is a placeholder for an
**official DocumentDB-owned Scarf organization** that must be registered before
release (see Open Issues). Until registered, requests to it fail silently and
harmlessly.

### Technical Details

**Module.** A single new module, `telemetry/scarf.rs`, contains all of:

- `ScarfOptions` (JSON config, `PascalCase` serde) and `ScarfConfig` (runtime
  config with the resolution/opt-out logic above).
- Process-wide shadow counters (`AtomicU64`) for operations and the four
  document-throughput counts, plus a global `AtomicBool` enable gate.
- `record_operation()` and `record_document_deltas(...)`: called from the
  request path; each is a single relaxed atomic load returning immediately when
  disabled, so there is negligible cost when telemetry is off.
- `init_scarf_telemetry(&ScarfConfig)`: no-op when disabled/opted-out; otherwise
  flips the gate, spawns a detached task that sends the launch event, and spawns
  a detached interval task that drains the counters and sends a summary (skips
  empty intervals).
- A shared `reqwest` client with a 3-second timeout.

**Counter sourcing.** The Scarf shadow counters are incremented from the same
place the OTLP document/operation counters are recorded, so both signals derive
from identical events. Per-request recording therefore runs when **either** OTLP
metrics **or** Scarf telemetry is enabled. (Prior to this change, per-request
recording was gated solely on the OTLP toggle; enabling Scarf alone would have
left the summary empty. This coupling is corrected so the two toggles are
independent.)

**Fire-and-forget safety.** All network work runs on detached Tokio tasks with a
3-second timeout; every error is swallowed and logged at debug only. A slow,
unreachable, blocked, or non-existent telemetry endpoint has **no effect** on
the database or on request latency.

### API Changes

- No changes to any user-facing database API, wire protocol, or UDFs.
- New public Rust items in `documentdb_gateway_core::telemetry`:
  `ScarfConfig`, `ScarfOptions`, `init_scarf_telemetry`, `record_operation`,
  `record_document_deltas`. These are internal gateway APIs, not database APIs.

### Database Schema Changes

None.

### Configuration Changes

- New `TelemetryOptions.Scarf` JSON section (`Enabled`, `Endpoint`,
  `SummaryIntervalMs`).
- New environment variables: `SCARF_ANALYTICS_ENABLED`,
  `SCARF_TELEMETRY_ENDPOINT`, `SCARF_SUMMARY_INTERVAL_MS`.
- Honors existing/standard `DO_NOT_TRACK` and `SCARF_NO_ANALYTICS`.
- New dependency: `reqwest` (HTTP client), built with the platform-native TLS
  stack to avoid adding an OpenSSL requirement on platforms that lack one.

### Dependency and Build Impact

- Adds `reqwest` to the gateway workspace. Scarf accepts only plain HTTP(S);
  the existing OTLP exporter uses gRPC/tonic and cannot target a Scarf endpoint,
  so a lightweight HTTP client is required.
- No change to build tooling or the runtime image layout; the emitter compiles
  into the existing gateway binary.

### Cost

- **Ingestion / event volume:** free. Scarf does not charge for events,
  telemetry volume, or download traffic, at any volume — this covers both the
  runtime launch/summary events and the container pull tracking.
- **Included free tier (Starter):** unlimited packages, unlimited download
  tracking, unlimited seats, a rolling ~3-month data window, plus a small
  monthly allotment of "Company Unlocks" and "Runs."
- **Optional paid consumption (not required for adoption numbers):**
  - *Company Unlocks* — to reveal the specific named company behind traffic
    (tiered, ~$3 each at low volume; a few free per month).
  - *Runs* — automated workflows such as exports, API calls, scheduled CRM
    syncs (tiered, ~$0.60 each at low volume; a small number free per month).
  - *Annual tiers* (e.g., higher committed volumes, longer data retention, raw
    data feeds) exist but are unnecessary for basic adoption reporting.
- **Net:** producing the OSS adoption numbers this RFC targets costs **$0**.
  Spend is only incurred if the project later opts into enrichment (which
  company) or automated exports.
- Worth evaluating: Scarf's Foundation-Backed Projects program, which may add
  free/discounted allowances.

### Testing Strategy

- **Unit tests** (in `scarf.rs`): default-disabled; enabled via JSON; opt-out
  overrides explicit enable; endpoint default vs. override; counters are no-ops
  when disabled and accumulate correctly when enabled.
- **Unit tests** (in `metrics.rs`): `hash_identifier` hashes real names to
  stable 16-hex tokens, is deterministic, distinct inputs → distinct outputs,
  and preserves sentinels (`""`, `"unknown"`).
- **Integration test:** with telemetry enabled and pointed at a local HTTP
  sink, assert a `emulator_launch` event on startup and a
  `gateway_metrics_summary` after activity within one interval; assert **no**
  events are emitted when disabled or when `DO_NOT_TRACK=1`.
- **Privacy assertion:** verify emitted payloads contain none of: database name,
  collection name, user name, document content.

### Migration Path

- **Backwards compatible / additive.** Default behavior is unchanged: with no
  new configuration, no events are sent.
- **Rollout of download tracking** requires updating the documented `docker
  pull` command to the Scarf domain. This should be done under an official
  DocumentDB-owned Scarf domain; the image path after the domain must exactly
  match the registry path (a Scarf/OCI requirement).
- **Rollback:** disable via env/JSON (or ship with default off), and revert the
  documented pull command to the direct registry URL. No data migration.

### Documentation Updates

- New top-level `TELEMETRY.md` describing, in full: the two separate systems,
  the exact two events and every field, what is never collected, all
  configuration/opt-out controls, and design guarantees.
- README: note telemetry is optional and off by default, link to `TELEMETRY.md`,
  and (at rollout) present the Scarf-fronted pull command for download tracking.
- CONTRIBUTING/SECURITY as needed to reference the telemetry policy.

---

## Implementation Tracking

*This section SHALL be populated during the Implementation phase.*

### Implementation PRs

- [ ] PR #XXX: Add `telemetry/scarf.rs` (config, counters, launch + summary emitter)
- [ ] PR #XXX: Integrate `Scarf` into `TelemetryConfig` / `TelemetryOptions`
- [ ] PR #XXX: Hash user-defined identifiers in gateway metrics (`hash_identifier`)
- [ ] PR #XXX: Decouple per-request recording so Scarf works without OTLP enabled
- [ ] PR #XXX: Initialize Scarf telemetry at gateway startup
- [ ] PR #XXX: Add `TELEMETRY.md` and README/CONTRIBUTING updates
- [ ] PR #XXX: Register official DocumentDB Scarf domain; update documented pull command

### Status Updates

**2026-07-29:** RFC drafted. Prototype emitter, identifier hashing, and the
per-request recording decoupling implemented and validated end-to-end against a
local sink (launch + aggregated summary observed; disabled/opt-out paths send
nothing).

### Open Questions

- [ ] **Official Scarf organization/domain.** Who owns the
      `documentdb.gateway.scarf.sh` domain and the Event Collection package?
      This must be an org-owned account (not a personal one) before release.
- [ ] **Default summary interval.** Is hourly the right cadence, or should the
      first release use a longer interval (e.g., daily) to minimize traffic?
- [ ] **Enablement policy.** Ship off-by-default only, or off-by-default with a
      prominent first-run notice? (This RFC assumes off-by-default, opt-in.)
- [ ] **Namespace consistency.** The image is published under more than one
      registry namespace; download tracking must front whichever path the
      README advertises (or track multiple).
- [ ] **Foundation program.** Does DocumentDB qualify for Scarf's
      Foundation-Backed Projects allowances?

### Implementation Notes

- **Decision [2026-07-29]: Separate Scarf emitter from OTLP.**
  - **Context:** OTLP metrics are high-frequency operational data for operators;
    Scarf events are coarse adoption signals for maintainers. Scarf accepts only
    HTTP, not OTLP/gRPC.
  - **Alternatives:** Bridging OTLP metrics into Scarf (rejected: wrong shape,
    high cardinality, values become strings in Scarf, and it would leak
    user-defined labels).

- **Decision [2026-07-29]: Hash user-defined identifiers at the source.**
  - **Context:** Ensures raw database/collection names never enter any telemetry
    attribute, including operators' own OTLP backends.
  - **Tradeoff:** Operators lose plaintext names in their own metrics. Accepted
    as a privacy-first default; a boundary-only hashing variant could be
    considered later if operators need plaintext locally.

- **Decision [2026-07-29]: Off by default, opt-in, fire-and-forget.**
  - **Context:** Runtime telemetry from a user's process must never surprise,
    block, or fail the workload.
  - **Result:** Disabled unless explicitly enabled; `DO_NOT_TRACK` /
    `SCARF_NO_ANALYTICS` always win; 3-second timeout; all errors ignored.
