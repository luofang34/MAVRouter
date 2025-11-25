//! Message deduplication logic for MAVLink messages.
//!
//! This module provides a mechanism to prevent processing duplicate messages
//! within a specified time window. It's useful for filtering out redundant
//! retransmissions or messages generated too frequently by sources.
//!
//! Two implementations are provided:
//! - `Dedup`: Single-threaded time-wheel based deduplication (requires external Mutex)
//! - `ConcurrentDedup`: Sharded concurrent deduplication for multicore scalability

use ahash::AHasher;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tracing::trace;

/// Calculate a hash for the given payload using `ahash`.
#[inline(always)]
fn ahash_hash(payload: &[u8]) -> u64 {
    let mut hasher = AHasher::default();
    payload.hash(&mut hasher);
    hasher.finish()
}

/// Implements message deduplication based on message payload and a time window.
///
/// This version uses a time-wheel approach to eliminate O(k) cleanup per message.
/// Cleanup is handled by a background task that rotates the "buckets".
pub struct Dedup {
    dedup_period: Duration,
    /// Buckets for the time wheel. Each bucket represents a slice of the `dedup_period`.
    /// Stores hashes of messages seen within that bucket's time slice.
    buckets: Vec<HashSet<u64, ahash::RandomState>>,
    /// The index of the currently active bucket where new messages are inserted.
    current_bucket: usize,
    /// The time interval each bucket represents.
    #[allow(dead_code)] // Used indirectly via ConcurrentDedup
    bucket_interval: Duration,
    /// Total number of buckets in the time wheel.
    num_buckets: usize,
}

impl Dedup {
    /// Creates a new `Dedup` instance with the specified deduplication period.
    ///
    /// The `dedup_period` is divided into several smaller time `buckets`.
    ///
    /// # Arguments
    ///
    /// * `dedup_period` - The duration within which messages are considered duplicates.
    ///   If `Duration::ZERO`, deduplication is effectively disabled.
    ///
    /// # Panics
    ///
    /// Panics if `dedup_period` is non-zero but too small to create at least 2 buckets.
    pub fn new(dedup_period: Duration) -> Self {
        if dedup_period.is_zero() {
            return Self {
                dedup_period,
                buckets: Vec::new(),
                current_bucket: 0,
                bucket_interval: Duration::ZERO,
                num_buckets: 0,
            };
        }

        // Use a default bucket interval, e.g., 100ms.
        // Ensure at least 2 buckets for proper time wheel rotation.
        let bucket_interval = Duration::from_millis(100);
        let mut num_buckets = (dedup_period.as_millis() / bucket_interval.as_millis()) as usize;
        num_buckets = num_buckets.max(2); // Ensure at least 2 buckets

        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            buckets.push(HashSet::with_hasher(ahash::RandomState::new()));
        }

        trace!(
            "Dedup: new instance with dedup_period {:?}, num_buckets {}, bucket_interval {:?}",
            dedup_period,
            num_buckets,
            bucket_interval
        );

        Self {
            dedup_period,
            buckets,
            current_bucket: 0,
            bucket_interval,
            num_buckets,
        }
    }

    /// Returns the configured interval for bucket rotation.
    #[allow(dead_code)] // Used in tests
    pub fn rotation_interval(&self) -> Duration {
        self.bucket_interval
    }

    /// Checks if a given message payload is a duplicate within the deduplication period.
    ///
    /// This method calculates a hash of the `payload` and checks if it exists in any
    /// of the active time-wheel buckets. This is an O(number_of_buckets) operation
    /// on average, but without per-call cleanup.
    ///
    /// # Arguments
    ///
    /// * `payload` - The byte slice representing the MAVLink message payload.
    ///
    /// # Returns
    ///
    /// `true` if the message is a duplicate, `false` otherwise.
    #[allow(dead_code)] // Used in tests
    pub fn is_duplicate(&self, payload: &[u8]) -> bool {
        if self.dedup_period.is_zero() {
            return false;
        }

        let hash = ahash_hash(payload);
        // Check all buckets if any contain the hash
        self.buckets.iter().any(|b| b.contains(&hash))
    }

    /// Inserts a message payload's hash into the current deduplication bucket.
    ///
    /// This method assumes `is_duplicate` has already been called and returned `false`,
    /// or that the caller explicitly wants to insert the message.
    ///
    /// # Arguments
    ///
    /// * `payload` - The byte slice representing the MAVLink message payload.
    #[allow(dead_code)] // Used in tests
    pub fn insert(&mut self, payload: &[u8]) {
        if self.dedup_period.is_zero() {
            return;
        }
        let hash = ahash_hash(payload);
        self.buckets[self.current_bucket].insert(hash);
    }

    /// Combined check and insert operation to avoid hashing twice.
    ///
    /// This is more efficient than calling `is_duplicate()` followed by `insert()`
    /// as it only computes the hash once.
    ///
    /// # Arguments
    ///
    /// * `payload` - The byte slice representing the MAVLink message payload.
    ///
    /// # Returns
    ///
    /// `true` if the message was a duplicate (not inserted), `false` if it was new (inserted).
    #[inline]
    #[allow(dead_code)] // Used in tests and by ConcurrentDedup internally
    pub fn check_and_insert(&mut self, payload: &[u8]) -> bool {
        if self.dedup_period.is_zero() {
            return false;
        }

        let hash = ahash_hash(payload);

        // Check all buckets if any contain the hash
        if self.buckets.iter().any(|b| b.contains(&hash)) {
            return true; // Duplicate
        }

        // Not a duplicate, insert into current bucket
        self.buckets[self.current_bucket].insert(hash);
        false
    }

    /// Rotates the time wheel to the next bucket.
    ///
    /// This method should be called periodically by a background task, with an
    /// interval equal to `self.rotation_interval()`. It clears the bucket
    /// that is now "oldest" (and thus outside the `dedup_period` window) and
    /// makes it the new `current_bucket`.
    pub fn rotate_bucket(&mut self) {
        if self.dedup_period.is_zero() {
            return;
        }

        // Advance to the next bucket
        self.current_bucket = (self.current_bucket + 1) % self.num_buckets;

        // Clear the new current bucket (which was previously the oldest)
        self.buckets[self.current_bucket].clear();
        trace!("Dedup: Rotated to bucket {}", self.current_bucket);
    }
}

/// Number of shards for concurrent deduplication.
/// Should be a power of 2 for efficient modulo via bitmask.
const NUM_SHARDS: usize = 16;

/// Concurrent message deduplication using sharding for multicore scalability.
///
/// This implementation partitions the deduplication state into multiple independent
/// shards, each protected by its own lock. Messages are routed to shards based on
/// their hash, allowing concurrent operations on different shards.
///
/// This design scales well on multicore systems as contention is reduced to
/// 1/NUM_SHARDS compared to a single global lock.
#[derive(Clone)]
pub struct ConcurrentDedup {
    /// Sharded dedup instances, each with its own lock.
    shards: Arc<[Mutex<Dedup>; NUM_SHARDS]>,
    /// Cached dedup period for quick disabled check.
    dedup_period: Duration,
    /// Bucket rotation interval (same for all shards).
    bucket_interval: Duration,
}

impl ConcurrentDedup {
    /// Creates a new `ConcurrentDedup` instance with the specified deduplication period.
    ///
    /// # Arguments
    ///
    /// * `dedup_period` - The duration within which messages are considered duplicates.
    ///   If `Duration::ZERO`, deduplication is effectively disabled.
    pub fn new(dedup_period: Duration) -> Self {
        // Create array of shards
        let shards: [Mutex<Dedup>; NUM_SHARDS] =
            std::array::from_fn(|_| Mutex::new(Dedup::new(dedup_period)));

        let bucket_interval = if dedup_period.is_zero() {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };

        trace!(
            "ConcurrentDedup: new instance with {} shards, dedup_period {:?}",
            NUM_SHARDS,
            dedup_period
        );

        Self {
            shards: Arc::new(shards),
            dedup_period,
            bucket_interval,
        }
    }

    /// Returns the configured interval for bucket rotation.
    #[inline]
    pub fn rotation_interval(&self) -> Duration {
        self.bucket_interval
    }

    /// Combined check and insert operation for concurrent access.
    ///
    /// Routes the message to a shard based on its hash, then performs
    /// the check-and-insert atomically within that shard.
    ///
    /// # Arguments
    ///
    /// * `payload` - The byte slice representing the MAVLink message payload.
    ///
    /// # Returns
    ///
    /// `true` if the message was a duplicate (not inserted), `false` if it was new (inserted).
    #[inline]
    pub fn check_and_insert(&self, payload: &[u8]) -> bool {
        if self.dedup_period.is_zero() {
            return false;
        }

        // Compute hash once, use it for both shard selection and dedup check
        let hash = ahash_hash(payload);
        let shard_idx = (hash as usize) & (NUM_SHARDS - 1); // Fast modulo for power of 2

        // Lock only this shard
        let mut shard = self.shards[shard_idx].lock();
        shard.check_and_insert_with_hash(hash)
    }

    /// Rotates all shards to the next bucket.
    ///
    /// This should be called periodically by a background task.
    pub fn rotate_buckets(&self) {
        if self.dedup_period.is_zero() {
            return;
        }

        for shard in self.shards.iter() {
            shard.lock().rotate_bucket();
        }
        trace!("ConcurrentDedup: Rotated all {} shards", NUM_SHARDS);
    }
}

impl Dedup {
    /// Internal method: check and insert using a pre-computed hash.
    /// Used by ConcurrentDedup to avoid double hashing.
    #[inline]
    fn check_and_insert_with_hash(&mut self, hash: u64) -> bool {
        if self.dedup_period.is_zero() {
            return false;
        }

        // Check all buckets if any contain the hash
        if self.buckets.iter().any(|b| b.contains(&hash)) {
            return true; // Duplicate
        }

        // Not a duplicate, insert into current bucket
        self.buckets[self.current_bucket].insert(hash);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_dedup_time_wheel() {
        let dedup_period = Duration::from_millis(500); // 500ms dedup period
        let bucket_interval = Duration::from_millis(100); // 100ms bucket interval
        let mut dedup = Dedup::new(dedup_period);
        assert_eq!(dedup.rotation_interval(), bucket_interval);
        assert_eq!(dedup.num_buckets, 5); // 500ms / 100ms = 5 buckets

        let data1 = b"hello";
        let data2 = b"world";

        // Insert data1
        assert!(!dedup.is_duplicate(data1));
        dedup.insert(data1);

        // data1 should be a duplicate now
        assert!(dedup.is_duplicate(data1));
        // data2 should not
        assert!(!dedup.is_duplicate(data2));

        // Rotate buckets until data1 is removed
        // It takes num_buckets rotations for an entry to fall out if it's not re-inserted
        for i in 0..dedup.num_buckets {
            println!("Rotate {} / {}", i + 1, dedup.num_buckets);
            dedup.rotate_bucket();
            // Data1 should still be a duplicate as it's within the window if re-inserted earlier.
            // After all buckets have rotated, the original slot for data1 should have been cleared.
            if i < dedup.num_buckets - 1 {
                assert!(
                    dedup.is_duplicate(data1),
                    "Data1 should be duplicate after {} rotations",
                    i
                );
            } else {
                assert!(
                    !dedup.is_duplicate(data1),
                    "Data1 should NOT be duplicate after {} rotations",
                    i
                );
            }
        }

        // Let's re-run a simple one where it expires
        let dedup_period = Duration::from_millis(200);
        let mut dedup_exp = Dedup::new(dedup_period); // 2 buckets (200ms / 100ms)
        assert_eq!(dedup_exp.num_buckets, 2);

        let data = b"expire_test";
        assert!(!dedup_exp.is_duplicate(data));
        dedup_exp.insert(data);
        assert!(dedup_exp.is_duplicate(data));

        // Rotate once: data should still be in the other bucket (1st bucket)
        thread::sleep(dedup_exp.rotation_interval());
        dedup_exp.rotate_bucket();
        assert!(dedup_exp.is_duplicate(data));

        // Rotate again: data's bucket should be cleared
        thread::sleep(dedup_exp.rotation_interval());
        dedup_exp.rotate_bucket();
        assert!(!dedup_exp.is_duplicate(data));

        // Test with dedup_period ZERO
        let mut dedup_zero = Dedup::new(Duration::ZERO);
        assert!(!dedup_zero.is_duplicate(data1));
        dedup_zero.insert(data1);
        assert!(!dedup_zero.is_duplicate(data1));
    }
}
