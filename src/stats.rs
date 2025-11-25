//! Statistics history tracking for MAVLink router.
//!
//! This module provides structures and methods to track and aggregate
//! routing statistics over time windows.

use crate::routing::RoutingStats;
use std::collections::VecDeque;

/// Simple Stats buffer history
pub struct StatsHistory {
    /// Recent N seconds samples
    pub samples: VecDeque<RoutingStats>,
    /// Max retention time in seconds
    pub max_age_secs: u64,
}

impl StatsHistory {
    /// Creates a new `StatsHistory` with the specified maximum retention time.
    pub fn new(max_age_secs: u64) -> Self {
        let capacity = max_age_secs as usize;
        Self {
            samples: VecDeque::with_capacity(capacity),
            max_age_secs,
        }
    }

    /// Add sample and clean up old data.
    pub fn push(&mut self, stats: RoutingStats) {
        self.samples.push_back(stats);

        if let Some(latest) = self.samples.back() {
            // Clean up old data exceeding max_age
            let cutoff = latest.timestamp.saturating_sub(self.max_age_secs);
            while let Some(oldest) = self.samples.front() {
                if oldest.timestamp < cutoff {
                    self.samples.pop_front();
                } else {
                    break;
                }
            }
        }
    }

    /// Calculate aggregated statistics for a specified time window.
    ///
    /// Uses binary search to efficiently find the start of the time window.
    pub fn aggregate(&self, window_secs: u64) -> Option<AggregatedStats> {
        if self.samples.is_empty() {
            return None;
        }

        let latest = self.samples.back()?;
        let cutoff = latest.timestamp.saturating_sub(window_secs);

        // Optimization: Use binary search (partition_point) to find the start index
        // because timestamps are strictly monotonically increasing.
        // partition_point returns the index of the first element where the predicate is false.
        // Predicate: timestamp < cutoff.
        // So it finds the first element where timestamp >= cutoff.
        let start_idx = self.samples.partition_point(|s| s.timestamp < cutoff);

        if start_idx >= self.samples.len() {
            return None;
        }

        let window = self.samples.range(start_idx..);
        let window_len = self.samples.len() - start_idx;

        if window_len == 0 {
            return None;
        }

        let sum_routes: usize = window.clone().map(|s| s.total_routes).sum();
        let avg_routes = sum_routes as f64 / window_len as f64;

        // We can use the iterator directly for min/max
        let max_routes = window.clone().map(|s| s.total_routes).max()?;
        let min_routes = window.clone().map(|s| s.total_routes).min()?;

        Some(AggregatedStats {
            avg_routes,
            max_routes,
            min_routes,
            sample_count: window_len,
        })
    }
}

/// Simplified aggregated stats
#[derive(Debug)]
pub struct AggregatedStats {
    /// Average number of routes in the window
    pub avg_routes: f64,
    /// Maximum number of routes in the window
    pub max_routes: usize,
    /// Minimum number of routes in the window
    pub min_routes: usize,
    /// Number of samples in the window
    pub sample_count: usize,
}

#[cfg(test)]
#[allow(clippy::expect_used)] // Allow expect() in tests for descriptive failure messages
mod tests {
    use super::*;
    use crate::routing::RoutingStats;

    fn dummy_stats(
        routes: usize,
        systems: usize,
        endpoints: usize,
        timestamp: u64,
    ) -> RoutingStats {
        RoutingStats {
            total_routes: routes,
            total_systems: systems,
            total_endpoints: endpoints,
            timestamp,
        }
    }

    #[test]
    fn test_stats_history_retention() {
        let mut history = StatsHistory::new(60); // 1 minute retention

        // Add 30 samples, 1 second apart
        for i in 0..30 {
            history.push(dummy_stats(i, i, i, i as u64));
        }
        assert_eq!(history.samples.len(), 30);

        // Add 40 more samples, total 70 samples. Max age is 60 secs.
        // Expect samples 0-9 to be pruned.
        for i in 30..70 {
            history.push(dummy_stats(i, i, i, i as u64));
        }
        assert_eq!(history.samples.len(), 61); // Should retain samples 9 to 69 (61 samples)

        // Verify oldest remaining sample
        assert_eq!(
            history
                .samples
                .front()
                .expect("History should not be empty")
                .timestamp,
            9
        );
        assert_eq!(
            history
                .samples
                .back()
                .expect("History should not be empty")
                .timestamp,
            69
        );

        // Test push with 0 retention (effectively disabled)
        let mut history_zero_retention = StatsHistory::new(0);
        history_zero_retention.push(dummy_stats(1, 1, 1, 1));
        history_zero_retention.push(dummy_stats(2, 2, 2, 2));
        assert_eq!(
            history_zero_retention.samples.len(),
            1,
            "0 retention should only keep the latest sample"
        );
    }

    #[test]
    fn test_aggregate_window() {
        let mut history = StatsHistory::new(100); // More than enough retention

        // Sample every second
        // Routes: 10, 20, 30, 40, 50 (timestamps 0-4)
        for i in 0..5 {
            history.push(dummy_stats((i + 1) * 10, 0, 0, i as u64));
        }
        // Current state: [t0:10, t1:20, t2:30, t3:40, t4:50]

        // Aggregate 3-second window (t2, t3, t4)
        // Values: 20, 30, 40, 50 (timestamps 1-4)
        // sum = 140, count = 4, avg = 35.0, min = 20, max = 50
        let agg_3s = history
            .aggregate(3)
            .expect("Aggregation should not be None");
        assert_eq!(agg_3s.sample_count, 4);
        assert_eq!(agg_3s.avg_routes, 35.0);
        assert_eq!(agg_3s.min_routes, 20);
        assert_eq!(agg_3s.max_routes, 50);

        // Aggregate 5-second window (all samples)
        // Values: 10, 20, 30, 40, 50
        // sum = 150, count = 5, avg = 30.0, min = 10, max = 50
        let agg_5s = history
            .aggregate(5)
            .expect("Aggregation should not be None");
        assert_eq!(agg_5s.sample_count, 5);
        assert_eq!(agg_5s.avg_routes, 30.0);
        assert_eq!(agg_5s.min_routes, 10);
        assert_eq!(agg_5s.max_routes, 50);

        // Aggregate a window before any samples
        assert!(history.aggregate(0).is_some()); // Latest sample's timestamp - 0 should include just the last one
        let last_sample_agg = history
            .aggregate(0)
            .expect("Aggregation for 0-window should not be None"); // Should be only the last sample if 0-window is used as (latest - 0)
        assert_eq!(last_sample_agg.sample_count, 1);
        assert_eq!(last_sample_agg.avg_routes, 50.0);

        // Aggregate a window larger than retention, should cover all
        let agg_large = history
            .aggregate(100)
            .expect("Aggregation for large window should not be None");
        assert_eq!(agg_large.sample_count, 5);
    }

    #[test]
    fn test_empty_history_aggregation() {
        let history = StatsHistory::new(60);
        assert!(history.aggregate(60).is_none());
    }
}
