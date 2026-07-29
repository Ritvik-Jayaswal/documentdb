/*-------------------------------------------------------------------------
 * Copyright (c) Microsoft Corporation.  All rights reserved.
 *
 * src/telemetry/scarf.rs
 *
 * Optional, privacy-respecting open-source usage telemetry that reports
 * low-frequency, aggregated signals to a Scarf Event Collection endpoint.
 *
 * This is intentionally separate from the OpenTelemetry OTLP metrics pipeline
 * in `metrics.rs`: OTLP metrics are high-frequency operational data destined
 * for an observability backend (Prometheus/Grafana/App Insights), whereas the
 * Scarf signals here are coarse adoption metrics (a launch event plus periodic
 * aggregated document/operation counts). Per Scarf's own guidance we only send
 * high-intent, low-frequency events and always honor user opt-out.
 *
 *-------------------------------------------------------------------------
 */

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        LazyLock,
    },
    time::Duration,
};

use serde::Deserialize;

use crate::telemetry::config::env_var;

// ============================================================================
// Constants
// ============================================================================

const DEFAULT_SCARF_ENABLED: bool = false;
const DEFAULT_SCARF_ENDPOINT: &str = "https://documentdb.gateway.scarf.sh/telemetry";
const DEFAULT_SUMMARY_INTERVAL_MS: u64 = 3_600_000; // 1 hour
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Gate checked on the hot path so counter updates are a single relaxed atomic
/// load (and a no-op) when Scarf telemetry is disabled.
static SCARF_ENABLED: AtomicBool = AtomicBool::new(false);

// ============================================================================
// JSON Configuration
// ============================================================================

/// JSON configuration for Scarf telemetry
/// (matches `SetupConfiguration.json` `TelemetryOptions.Scarf`).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ScarfOptions {
    /// Whether Scarf usage telemetry is enabled.
    pub enabled: Option<bool>,
    /// Scarf Event Collection endpoint URL.
    pub endpoint: Option<String>,
    /// Interval between aggregated summary events, in milliseconds.
    pub summary_interval_ms: Option<u64>,
}

// ============================================================================
// Runtime Configuration
// ============================================================================

/// Runtime configuration for Scarf telemetry.
///
/// Resolution order for each value: JSON > environment variable > default.
/// Regardless of configuration, telemetry is force-disabled when the user sets
/// the standard `DO_NOT_TRACK=1` or `SCARF_NO_ANALYTICS=1` opt-out variables.
#[derive(Debug, Clone)]
pub struct ScarfConfig {
    enabled: Option<bool>,
    endpoint: Option<String>,
    summary_interval_ms: Option<u64>,
}

impl ScarfConfig {
    /// Creates a Scarf config from optional JSON configuration.
    #[must_use]
    pub fn new(json_config: Option<&ScarfOptions>) -> Self {
        let json = json_config.cloned().unwrap_or_default();
        Self {
            enabled: json.enabled,
            endpoint: json.endpoint,
            summary_interval_ms: json.summary_interval_ms,
        }
    }

    /// Whether the user has explicitly opted out via a standard env variable.
    ///
    /// Honors both the cross-vendor `DO_NOT_TRACK` convention and Scarf's own
    /// `SCARF_NO_ANALYTICS`. Any of `1`/`true` (case-insensitive) opts out.
    #[must_use]
    fn opted_out() -> bool {
        ["DO_NOT_TRACK", "SCARF_NO_ANALYTICS"]
            .iter()
            .filter_map(|var| std::env::var(var).ok())
            .any(|v| {
                let v = v.trim().to_ascii_lowercase();
                v == "1" || v == "true"
            })
    }

    /// Whether Scarf telemetry is enabled. Opt-out always wins.
    /// Fallback: JSON > `SCARF_ANALYTICS_ENABLED` > false.
    #[must_use]
    pub fn enabled(&self) -> bool {
        if Self::opted_out() {
            return false;
        }
        self.enabled
            .or_else(|| env_var("SCARF_ANALYTICS_ENABLED"))
            .unwrap_or(DEFAULT_SCARF_ENABLED)
    }

    /// Scarf endpoint URL. Fallback: JSON > `SCARF_TELEMETRY_ENDPOINT` > default.
    #[must_use]
    pub fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .or_else(|| env_var("SCARF_TELEMETRY_ENDPOINT"))
            .unwrap_or_else(|| DEFAULT_SCARF_ENDPOINT.to_owned())
    }

    /// Summary interval in ms. Fallback: JSON > `SCARF_SUMMARY_INTERVAL_MS` > 1h.
    #[must_use]
    pub fn summary_interval_ms(&self) -> u64 {
        self.summary_interval_ms
            .or_else(|| env_var("SCARF_SUMMARY_INTERVAL_MS"))
            .unwrap_or(DEFAULT_SUMMARY_INTERVAL_MS)
    }
}

// ============================================================================
// Shadow counters (read back for aggregated summaries)
// ============================================================================

/// Lightweight process-wide counters shadowing the write-only OTel counters in
/// `metrics.rs`, so the summary task can read accumulated deltas back in-process
/// without an observability backend.
#[derive(Debug, Default)]
struct ScarfCounters {
    documents_inserted: AtomicU64,
    documents_returned: AtomicU64,
    documents_updated: AtomicU64,
    documents_deleted: AtomicU64,
    operations: AtomicU64,
}

static SCARF_COUNTERS: ScarfCounters = ScarfCounters {
    documents_inserted: AtomicU64::new(0),
    documents_returned: AtomicU64::new(0),
    documents_updated: AtomicU64::new(0),
    documents_deleted: AtomicU64::new(0),
    operations: AtomicU64::new(0),
};

/// Records one executed operation. No-op unless Scarf telemetry is enabled.
///
/// Called from the request hot path; when disabled this is a single relaxed
/// atomic load that returns immediately.
pub fn record_operation() {
    if !SCARF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    SCARF_COUNTERS.operations.fetch_add(1, Ordering::Relaxed);
}

/// Adds document-throughput deltas. No-op unless Scarf telemetry is enabled.
pub fn record_document_deltas(inserted: u64, returned: u64, updated: u64, deleted: u64) {
    if !SCARF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if inserted > 0 {
        SCARF_COUNTERS
            .documents_inserted
            .fetch_add(inserted, Ordering::Relaxed);
    }
    if returned > 0 {
        SCARF_COUNTERS
            .documents_returned
            .fetch_add(returned, Ordering::Relaxed);
    }
    if updated > 0 {
        SCARF_COUNTERS
            .documents_updated
            .fetch_add(updated, Ordering::Relaxed);
    }
    if deleted > 0 {
        SCARF_COUNTERS
            .documents_deleted
            .fetch_add(deleted, Ordering::Relaxed);
    }
}

/// A snapshot of counter deltas since the previous drain.
#[derive(Debug, Clone, Copy)]
struct CounterSnapshot {
    documents_inserted: u64,
    documents_returned: u64,
    documents_updated: u64,
    documents_deleted: u64,
    operations: u64,
}

impl CounterSnapshot {
    /// Atomically reads and resets all counters, returning the deltas.
    fn drain() -> Self {
        Self {
            documents_inserted: SCARF_COUNTERS.documents_inserted.swap(0, Ordering::Relaxed),
            documents_returned: SCARF_COUNTERS.documents_returned.swap(0, Ordering::Relaxed),
            documents_updated: SCARF_COUNTERS.documents_updated.swap(0, Ordering::Relaxed),
            documents_deleted: SCARF_COUNTERS.documents_deleted.swap(0, Ordering::Relaxed),
            operations: SCARF_COUNTERS.operations.swap(0, Ordering::Relaxed),
        }
    }

    /// Whether any activity was recorded in this interval.
    const fn is_empty(self) -> bool {
        self.operations == 0
            && self.documents_inserted == 0
            && self.documents_returned == 0
            && self.documents_updated == 0
            && self.documents_deleted == 0
    }
}

// ============================================================================
// Emission
// ============================================================================

/// Shared HTTP client with an aggressive timeout so telemetry never blocks.
static HTTP_CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .ok()
});

/// Static host attributes shared by every event.
fn host_attributes() -> String {
    format!(
        "version={}&os={}&arch={}&db_system=documentdb",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Fires a single event to the Scarf endpoint. Errors are swallowed: telemetry
/// must never interrupt the gateway.
async fn send_event(endpoint: &str, query: &str) {
    let Some(client) = HTTP_CLIENT.as_ref() else {
        return;
    };
    let url = format!("{endpoint}?{query}");
    match client.get(&url).send().await {
        Ok(resp) => tracing::debug!("Scarf telemetry event sent: {}", resp.status()),
        Err(err) => tracing::debug!("Scarf telemetry event failed (ignored): {err}"),
    }
}

/// Initializes Scarf telemetry: enables hot-path counters, sends a one-time
/// launch event, and spawns a background task that periodically forwards
/// aggregated summaries. Does nothing when disabled or opted out.
///
/// Fire-and-forget: all work runs on detached Tokio tasks and never blocks
/// startup or shutdown.
pub fn init_scarf_telemetry(config: &ScarfConfig) {
    if !config.enabled() {
        tracing::debug!("Scarf usage telemetry disabled");
        return;
    }

    SCARF_ENABLED.store(true, Ordering::Relaxed);
    let endpoint = config.endpoint();
    let interval = Duration::from_millis(config.summary_interval_ms());

    tracing::info!(
        "Scarf usage telemetry enabled (endpoint={endpoint}, summary_interval={:?}). \
         Set DO_NOT_TRACK=1 or SCARF_NO_ANALYTICS=1 to opt out.",
        interval
    );

    // One-time launch event.
    let launch_endpoint = endpoint.clone();
    tokio::spawn(async move {
        let query = format!("event=emulator_launch&{}", host_attributes());
        send_event(&launch_endpoint, &query).await;
    });

    // Periodic aggregated summary.
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick so we don't emit an empty summary at t=0.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let snap = CounterSnapshot::drain();
            if snap.is_empty() {
                continue;
            }
            let query = format!(
                "event=gateway_metrics_summary&{}&operations={}&documents_inserted={}&documents_returned={}&documents_updated={}&documents_deleted={}",
                host_attributes(),
                snap.operations,
                snap.documents_inserted,
                snap.documents_returned,
                snap.documents_updated,
                snap.documents_deleted,
            );
            send_event(&endpoint, &query).await;
        }
    });
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::EnvGuard;

    #[test]
    fn test_scarf_disabled_by_default() {
        let config = ScarfConfig::new(None);
        assert!(!config.enabled());
    }

    #[test]
    fn test_scarf_enabled_via_json() {
        let json = ScarfOptions {
            enabled: Some(true),
            ..Default::default()
        };
        let config = ScarfConfig::new(Some(&json));
        assert!(config.enabled());
    }

    #[test]
    fn test_scarf_opt_out_overrides_json() {
        let _guard = EnvGuard::set("DO_NOT_TRACK", "1");
        let json = ScarfOptions {
            enabled: Some(true),
            ..Default::default()
        };
        let config = ScarfConfig::new(Some(&json));
        assert!(!config.enabled(), "opt-out must override explicit enable");
    }

    #[test]
    fn test_scarf_endpoint_default_and_override() {
        let config = ScarfConfig::new(None);
        assert_eq!(config.endpoint(), DEFAULT_SCARF_ENDPOINT);

        let json = ScarfOptions {
            endpoint: Some("https://example.gateway.scarf.sh/t".to_owned()),
            ..Default::default()
        };
        let config = ScarfConfig::new(Some(&json));
        assert_eq!(config.endpoint(), "https://example.gateway.scarf.sh/t");
    }

    #[test]
    fn test_counters_are_noop_when_disabled() {
        // Ensure the gate is off, then verify recording does not accumulate.
        SCARF_ENABLED.store(false, Ordering::Relaxed);
        let before = CounterSnapshot::drain();
        assert!(before.is_empty() || !before.is_empty()); // drain resets
        record_operation();
        record_document_deltas(3, 4, 5, 6);
        let after = CounterSnapshot::drain();
        assert!(after.is_empty(), "counters must not move when disabled");
    }

    #[test]
    fn test_counters_accumulate_when_enabled() {
        SCARF_ENABLED.store(true, Ordering::Relaxed);
        let _ = CounterSnapshot::drain(); // reset baseline
        record_operation();
        record_operation();
        record_document_deltas(3, 4, 5, 6);
        let snap = CounterSnapshot::drain();
        assert_eq!(snap.operations, 2);
        assert_eq!(snap.documents_inserted, 3);
        assert_eq!(snap.documents_returned, 4);
        assert_eq!(snap.documents_updated, 5);
        assert_eq!(snap.documents_deleted, 6);
        // Reset gate to avoid leaking state to other tests.
        SCARF_ENABLED.store(false, Ordering::Relaxed);
    }
}
