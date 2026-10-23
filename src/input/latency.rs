//! Optional, fixed-memory input pipeline measurements. Percentiles are bucket
//! upper bounds (microseconds), not screen end-to-end latency measurements.
use std::fmt::Write;
use std::time::{Duration, Instant};

const BUCKET_US: [u64; 16] = [
    10, 25, 50, 100, 250, 500, 1_000, 2_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000,
    500_000, 1_000_000,
];
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub enum Stage {
    SendQueue,
    SendWrite,
    ReceiveQueue,
    Inject,
    PacingLate,
}

#[derive(Default)]
struct Histogram {
    buckets: [u64; 17],
    count: u64,
    max_us: u64,
}

impl Histogram {
    fn record(&mut self, duration: Duration) {
        // Round up so positive sub-microsecond durations remain visible.
        let micros = duration.as_nanos().div_ceil(1_000).min(u64::MAX as u128) as u64;
        let bucket = BUCKET_US
            .iter()
            .position(|&bound| micros <= bound)
            .unwrap_or(BUCKET_US.len());
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.max_us = self.max_us.max(micros);
    }

    fn percentile(&self, percent: u64) -> u64 {
        let rank = (u128::from(self.count) * u128::from(percent)).div_ceil(100);
        let mut seen = 0_u128;
        for (index, &count) in self.buckets.iter().enumerate() {
            seen += u128::from(count);
            if seen >= rank {
                return BUCKET_US.get(index).copied().unwrap_or(self.max_us);
            }
        }
        self.max_us
    }
}

pub struct LatencyStats {
    enabled: bool,
    label: &'static str,
    histograms: [Histogram; 5],
    coalesced_samples: u64,
    max_queue_depth: usize,
    last_report: Instant,
}

impl LatencyStats {
    pub fn from_env(label: &'static str) -> Self {
        Self::new(
            std::env::var("CROSSDESK_INPUT_METRICS").as_deref() == Ok("1"),
            label,
        )
    }

    pub fn new(enabled: bool, label: &'static str) -> Self {
        Self::new_at(enabled, label, Instant::now())
    }

    fn new_at(enabled: bool, label: &'static str, now: Instant) -> Self {
        Self {
            enabled,
            label,
            histograms: std::array::from_fn(|_| Histogram::default()),
            coalesced_samples: 0,
            max_queue_depth: 0,
            last_report: now,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn record(&mut self, stage: Stage, duration: Duration) {
        if self.enabled {
            self.histograms[stage as usize].record(duration);
        }
    }

    /// Observe one consumed queue record. `samples` includes its original event;
    /// the additional original events count as coalesced samples.
    pub fn queue(&mut self, depth: usize, samples: u64) {
        if self.enabled {
            self.max_queue_depth = self.max_queue_depth.max(depth);
            self.coalesced_samples = self
                .coalesced_samples
                .saturating_add(samples.saturating_sub(1));
        }
    }

    /// Return one aggregated observation window for the caller to log.
    pub fn maybe_report(&mut self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.report_at(Instant::now())
    }

    fn report_at(&mut self, now: Instant) -> Option<String> {
        if !self.enabled || now.saturating_duration_since(self.last_report) < REPORT_INTERVAL {
            return None;
        }
        self.last_report = now;
        if self.histograms.iter().all(|hist| hist.count == 0)
            && self.max_queue_depth == 0
            && self.coalesced_samples == 0
        {
            return None;
        }
        let mut report = format!(
            "input_metrics label={} percentile=bucket_upper_bound",
            self.label
        );
        for (name, histogram) in [
            "send_queue",
            "send_write",
            "receive_queue",
            "inject",
            "pacing_late",
        ]
        .into_iter()
        .zip(self.histograms.iter())
        {
            if histogram.count == 0 {
                continue;
            }
            let _ = write!(
                report,
                " {name} count={} p50_us={} p95_us={} p99_us={} max_us={}",
                histogram.count,
                histogram.percentile(50),
                histogram.percentile(95),
                histogram.percentile(99),
                histogram.max_us
            );
        }
        let _ = write!(
            report,
            " coalesced_samples={} max_queue_depth={}",
            self.coalesced_samples, self.max_queue_depth
        );
        self.histograms = std::array::from_fn(|_| Histogram::default());
        self.coalesced_samples = 0;
        self.max_queue_depth = 0;
        Some(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Catches lost samples, wrong nearest-rank percentiles, and queue accounting.
    #[test]
    fn reports_hand_checked_ranks_and_queue_totals() {
        let start = Instant::now();
        let mut stats = LatencyStats::new_at(true, "sender", start);
        for _ in 0..50 {
            stats.record(Stage::SendQueue, Duration::from_micros(10));
        }
        for _ in 0..45 {
            stats.record(Stage::SendQueue, Duration::from_micros(100));
        }
        for _ in 0..4 {
            stats.record(Stage::SendQueue, Duration::from_micros(1000));
        }
        stats.record(Stage::SendQueue, Duration::from_micros(5001));
        stats.queue(7, 4);
        stats.queue(2, 1);
        let report = stats.report_at(start + Duration::from_secs(5)).unwrap();
        assert!(
            report.contains("send_queue count=100 p50_us=10 p95_us=100 p99_us=1000 max_us=5001"),
            "{report}"
        );
        assert!(
            report.contains("coalesced_samples=3 max_queue_depth=7"),
            "{report}"
        );
    }

    // Catches reporting too early, failure to reset, and duplicate window output.
    #[test]
    fn reports_at_most_every_five_seconds_and_resets_the_window() {
        let start = Instant::now();
        let mut stats = LatencyStats::new_at(true, "receiver", start);
        stats.record(Stage::Inject, Duration::from_micros(20));
        assert!(
            stats
                .report_at(start + Duration::from_millis(4999))
                .is_none()
        );
        assert!(stats.report_at(start + Duration::from_secs(5)).is_some());
        stats.record(Stage::Inject, Duration::from_micros(40));
        assert!(
            stats
                .report_at(start + Duration::from_millis(9999))
                .is_none()
        );
        let report = stats.report_at(start + Duration::from_secs(10)).unwrap();
        assert!(
            report.contains("inject count=1 p50_us=50 p95_us=50 p99_us=50 max_us=40"),
            "{report}"
        );
        assert!(stats.report_at(start + Duration::from_secs(15)).is_none());
    }

    // Catches enabled checks omitted from recording or reporting.
    #[test]
    fn disabled_metrics_never_report() {
        let start = Instant::now();
        let mut stats = LatencyStats::new_at(false, "off", start);
        stats.record(Stage::SendWrite, Duration::from_secs(1));
        stats.queue(256, 99);
        assert!(stats.report_at(start + Duration::from_secs(100)).is_none());
    }

    // Catches overflow bucket omission and underreported duration maxima.
    #[test]
    fn overflow_and_submicrosecond_samples_remain_observable() {
        let start = Instant::now();
        let mut stats = LatencyStats::new_at(true, "sender", start);
        stats.record(Stage::PacingLate, Duration::from_secs(90));
        stats.record(Stage::ReceiveQueue, Duration::from_nanos(1));
        let report = stats.report_at(start + Duration::from_secs(5)).unwrap();
        assert!(report.contains("pacing_late count=1 p50_us=90000000 p95_us=90000000 p99_us=90000000 max_us=90000000"), "{report}");
        assert!(
            report.contains("receive_queue count=1 p50_us=10 p95_us=10 p99_us=10 max_us=1"),
            "{report}"
        );
    }
}
