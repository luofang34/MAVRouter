//! Exponential backoff helper used by the endpoint supervisor for restart delays.

use std::time::Duration;

/// Exponential backoff helper for connection retries.
#[derive(Debug)]
pub struct ExponentialBackoff {
    current: Duration,
    min: Duration,
    max: Duration,
    multiplier: f64,
}

impl ExponentialBackoff {
    /// Creates a new `ExponentialBackoff` instance.
    ///
    /// # Arguments
    ///
    /// * `min` - The minimum (initial) delay duration.
    /// * `max` - The maximum delay duration.
    /// * `multiplier` - The factor by which the delay increases after each call to `next()`.
    pub fn new(min: Duration, max: Duration, multiplier: f64) -> Self {
        Self {
            current: min,
            min,
            max,
            multiplier,
        }
    }

    /// Returns the current delay duration and updates it for the next call.
    ///
    /// The returned duration is the one that should be waited *now*. The internal
    /// state is updated to `current * multiplier` (capped at `max`) for the *next* call.
    pub fn next_backoff(&mut self) -> Duration {
        let wait = self.current;
        self.current = std::cmp::min(
            self.max,
            Duration::from_secs_f64(self.current.as_secs_f64() * self.multiplier),
        );
        wait
    }

    /// Resets the backoff delay to the minimum value.
    ///
    /// This should be called when a connection is successfully established or
    /// a task runs successfully for a sufficient duration.
    pub fn reset(&mut self) {
        self.current = self.min;
    }
}
