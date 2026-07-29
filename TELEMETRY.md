# DocumentDB Telemetry

DocumentDB can optionally report a small amount of **anonymous, aggregated
usage telemetry** so the maintainers can understand adoption of the open-source
project (how many deployments, on which versions and platforms). This document
explains — in full — what can be collected, when, how to see it, and how to turn
it off.

**Telemetry is disabled by default.** Nothing is sent unless you explicitly
enable it.

---

## TL;DR

| | |
|---|---|
| **Default state** | Off. No data leaves your machine. |
| **How to enable** | Set `SCARF_ANALYTICS_ENABLED=true`. |
| **How to disable / opt out** | Do nothing (off by default), or set `DO_NOT_TRACK=1` or `SCARF_NO_ANALYTICS=1`. Opt-out always wins. |
| **What is sent** | A launch ping + periodic aggregated counts (see below). |
| **What is never sent** | Your data, documents, queries, database names, collection names, user names, credentials, or IP-derived identity beyond what the receiving service infers from the request. |
| **Where it goes** | A [Scarf](https://scarf.sh) Event Collection endpoint (HTTPS). |
| **Cost to you** | None. |

---

## Two independent, unrelated telemetry systems

DocumentDB has two separate things that are sometimes both called "metrics."
They are **not** the same and are controlled independently.

### 1. Operational metrics — OpenTelemetry / OTLP (for operators, not maintainers)

The gateway can emit detailed **per-request operational metrics** (operation
latency, request/response sizes, document throughput, PostgreSQL phase timings)
over the standard **OpenTelemetry OTLP** protocol to an endpoint **you**
configure — e.g. your own Prometheus, Grafana, or Azure Monitor / Application
Insights.

- Controlled by `OTEL_METRICS_ENABLED` and the standard `OTEL_*` environment
  variables.
- **This data goes only where you point it.** It is never sent to the
  DocumentDB maintainers or to Scarf.
- User-defined identifiers in these metrics (database name, collection name) are
  **hashed** (SHA-256, truncated) before they are attached as metric attributes,
  so raw names do not appear even in your own observability backend. See
  `pg_documentdb_gw/documentdb_gateway_core/src/telemetry/metrics.rs`
  (`hash_identifier`).

This system exists for **you** to observe your own deployment. It is documented
here only to make clear that it is separate from the maintainer usage telemetry
described next.

### 2. Usage telemetry — Scarf (for maintainers)

A small, low-frequency, aggregated signal sent to the DocumentDB project's
usage-analytics endpoint so maintainers can gauge open-source adoption. This is
the telemetry the rest of this document is about. Source:
`pg_documentdb_gw/documentdb_gateway_core/src/telemetry/scarf.rs`.

---

## Exactly what the usage telemetry sends

When (and only when) usage telemetry is enabled, the gateway sends HTTPS
requests to a Scarf Event Collection endpoint. There are exactly two event
types.

### Event: `emulator_launch`

Sent **once**, shortly after the gateway starts.

| Field | Example | Meaning |
|-------|---------|---------|
| `event` | `emulator_launch` | Event type. |
| `version` | `0.104.0` | DocumentDB gateway version. |
| `os` | `linux` | Operating system (compile-time constant). |
| `arch` | `x86_64` | CPU architecture (compile-time constant). |
| `db_system` | `documentdb` | Constant identifier. |

### Event: `gateway_metrics_summary`

Sent **periodically** (default: once per hour) **only if there was activity**
in the interval. All values are **process-wide aggregate counts** for the
interval — never per-request, never per-collection.

| Field | Example | Meaning |
|-------|---------|---------|
| `event` | `gateway_metrics_summary` | Event type. |
| `version`, `os`, `arch`, `db_system` | (as above) | Same host attributes. |
| `operations` | `42` | Total operations handled in the interval. |
| `documents_inserted` | `10` | Documents inserted in the interval. |
| `documents_returned` | `55` | Documents returned by reads in the interval. |
| `documents_updated` | `7` | Documents updated in the interval. |
| `documents_deleted` | `3` | Documents deleted in the interval. |

That is the complete list. The payload is a plain HTTP query string; there is no
request body and no additional hidden fields.

---

## What is **never** collected

The usage telemetry is deliberately coarse and non-identifying. It does **not**
include, and the code never sends:

- **Your data** — no document contents, field names, or values.
- **Queries** — no filters, aggregation pipelines, or command arguments.
- **Names you chose** — no database names, collection names, index names, or
  user names.
- **Credentials** — no passwords, connection strings, tokens, or keys.
- **Network identity** — the gateway does not collect or send your IP address.
  (As with any HTTPS request, the receiving service sees the source address of
  the connection and may derive coarse geo/organization information from it;
  this is inherent to making a network request and is not something the gateway
  transmits in the payload.)
- **Per-request or per-collection breakdowns** — only whole-process aggregate
  counts are sent.

---

## How to control it

Resolution order for each setting: JSON config file > environment variable >
built-in default. User opt-out (`DO_NOT_TRACK` / `SCARF_NO_ANALYTICS`) overrides
everything.

| Setting | Env var | JSON (`TelemetryOptions.Scarf`) | Default |
|---------|---------|----------------------------------|---------|
| Enable | `SCARF_ANALYTICS_ENABLED` | `Enabled` | `false` (off) |
| Endpoint | `SCARF_TELEMETRY_ENDPOINT` | `Endpoint` | `https://documentdb.gateway.scarf.sh/telemetry` |
| Summary interval (ms) | `SCARF_SUMMARY_INTERVAL_MS` | `SummaryInterval Ms` | `3600000` (1 hour) |
| Opt out | `DO_NOT_TRACK=1` **or** `SCARF_NO_ANALYTICS=1` | — | not set |

### Opt out

Because telemetry is **off by default**, doing nothing already means no data is
sent. If you want to guarantee it stays off even if a configuration enables it,
set either standard opt-out variable:

```bash
export DO_NOT_TRACK=1
# or
export SCARF_NO_ANALYTICS=1
```

`DO_NOT_TRACK` follows the cross-vendor [Console Do Not Track](https://consoledonottrack.com/)
convention.

### Enable (Docker example)

```bash
docker run -dt -p 10260:10260 \
  -e SCARF_ANALYTICS_ENABLED=true \
  ghcr.io/documentdb/documentdb/documentdb-local:latest \
  --username <user> --password <pass>
```

---

## Design guarantees

- **Off by default.** Enabling is an explicit, opt-in action.
- **Never blocks or fails your workload.** Telemetry runs on detached background
  tasks with a short (3s) timeout, and any error is silently ignored. A failed
  or unreachable telemetry endpoint has no effect on the database.
- **Aggregated, not per-request.** Only whole-process counts over an interval
  are sent, following Scarf's guidance to prefer low-frequency, high-intent
  events.
- **Auditable.** The entire implementation is ~1 file:
  `pg_documentdb_gw/documentdb_gateway_core/src/telemetry/scarf.rs`. You can read
  exactly what is collected and sent.

---

## Distribution analytics (separate from the running gateway)

Independently of the gateway, the project may route its published container
image pulls (e.g. the documented `docker pull …` command) through a Scarf
gateway URL so maintainers can count downloads by version/platform. This happens
at **download** time via the registry redirect and is unrelated to any telemetry
emitted by a running DocumentDB instance. It collects no data from your
deployment.

---

## Why we collect this

DocumentDB is open source. Download counts alone can't distinguish a real,
running deployment from a CI cache pull. A tiny launch + aggregate-usage signal
helps maintainers understand real adoption, prioritize platforms/versions, and
justify continued investment — without collecting anything about your data or
identity. If you'd rather not participate, it's off by default and one variable
to keep it that way.
