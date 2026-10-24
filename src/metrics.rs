//! Prometheus-style metrics export (Tessellio).
//!
//! Renders codec telemetry as Prometheus text exposition format so a
//! deployment can scrape it.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::telemetry::{CodecEvent, Telemetry};

/// A telemetry sink that records Prometheus counters.
#[derive(Default)]
pub struct PrometheusTelemetry {
    encode_count: AtomicU64,
    encode_bytes: AtomicU64,
    decode_count: AtomicU64,
}

impl Telemetry for PrometheusTelemetry {
    fn record(&self, event: CodecEvent) {
        match event {
            CodecEvent::PhaseElapsed { .. } => {
                self.encode_count.fetch_add(1, Ordering::Relaxed);
            }
            CodecEvent::SelfCheckFailed { .. } => {
                self.decode_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl std::fmt::Debug for PrometheusTelemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrometheusTelemetry")
            .field("encode_count", &self.encode_count.load(Ordering::Relaxed))
            .field("encode_bytes", &self.encode_bytes.load(Ordering::Relaxed))
            .field("decode_count", &self.decode_count.load(Ordering::Relaxed))
            .finish()
    }
}

impl PrometheusTelemetry {
    /// Record encoded bytes (call alongside record).
    pub fn add_bytes(&self, n: u64) {
        self.encode_bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Render as Prometheus text exposition format.
    pub fn render(&self) -> String {
        format!(
            "# HELP tessellio_encode_total Total encode operations.\n\
             # TYPE tessellio_encode_total counter\n\
             tessellio_encode_total {}\n\
             # HELP tessellio_encode_bytes_total Total encoded bytes.\n\
             # TYPE tessellio_encode_bytes_total counter\n\
             tessellio_encode_bytes_total {}\n\
             # HELP tessellio_decode_total Total decode operations.\n\
             # TYPE tessellio_decode_total counter\n\
             tessellio_decode_total {}\n",
            self.encode_count.load(Ordering::Relaxed),
            self.encode_bytes.load(Ordering::Relaxed),
            self.decode_count.load(Ordering::Relaxed)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_metrics() {
        let t = PrometheusTelemetry::default();
        t.record(CodecEvent::PhaseElapsed {
            phase: crate::telemetry::EncodePhase::Total,
            elapsed: std::time::Duration::from_millis(1),
        });
        t.add_bytes(100);
        let out = t.render();
        assert!(out.contains("tessellio_encode_total 1"));
        assert!(out.contains("tessellio_encode_bytes_total 100"));
    }
}
