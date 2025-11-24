//! Message deduplication logic for MAVLink messages.
//!
//! This module provides a mechanism to prevent processing duplicate messages
//! within a specified time window. It's useful for filtering out redundant
//! retransmissions or messages generated too frequently by sources.

use ahash::AHasher;
use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// Implements message deduplication based on message payload and a time window.
///
/// Duplicate messages (same payload hash) received within the `dedup_period`
/// will be identified as duplicates.
pub struct Dedup {
    /// The time window within which messages are considered duplicates.
    dedup_period: Duration,
    /// A history of messages seen, used for cleaning up expired entries.
    history: VecDeque<(Instant, u64)>,
    /// A set of hashes of recently seen messages for quick lookup.
    set: HashSet<u64, ahash::RandomState>,
}

impl Dedup {
    /// Creates a new `Dedup` instance with the specified deduplication period.
    ///
    /// # Arguments
    ///
    /// * `dedup_period` - The duration within which messages are considered duplicates.
    ///   If `Duration::ZERO`, deduplication is effectively disabled.
    pub fn new(dedup_period: Duration) -> Self {
        Self {
            dedup_period,
            history: VecDeque::new(),
            set: HashSet::with_hasher(ahash::RandomState::new()),
        }
    }

    /// Checks if a given message payload is a duplicate within the deduplication period.
    ///
    /// This method calculates a hash of the `payload` and checks if it has been
    /// seen recently. It also automatically cleans up expired entries from its
    /// internal history.
    ///
    /// # Arguments
    ///
    /// * `payload` - The byte slice representing the MAVLink message payload.
    ///
    /// # Returns
    ///
    /// `true` if the message is a duplicate, `false` otherwise.
    pub fn is_duplicate(&mut self, payload: &[u8]) -> bool {
        if self.dedup_period.is_zero() {
            return false;
        }

        let now = Instant::now();

        // Cleanup old entries
        while let Some((timestamp, hash)) = self.history.front() {
            if now.duration_since(*timestamp) > self.dedup_period {
                self.set.remove(hash);
                self.history.pop_front();
            } else {
                break;
            }
        }

        // Calculate hash (using ahash for speed)
        let mut hasher = AHasher::default();
        payload.hash(&mut hasher);
        let hash = hasher.finish();

        if self.set.contains(&hash) {
            true
        } else {
            self.set.insert(hash);
            self.history.push_back((now, hash));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup() {
        let mut dedup = Dedup::new(Duration::from_millis(100));
        let data = b"hello";

        assert!(!dedup.is_duplicate(data));
        assert!(dedup.is_duplicate(data));

        std::thread::sleep(Duration::from_millis(110));
        assert!(!dedup.is_duplicate(data));
    }
}
