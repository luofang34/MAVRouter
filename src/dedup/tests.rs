//! Unit tests for [`Dedup`] and [`ConcurrentDedup`].
//!
//! Split out of `src/dedup.rs` into a sibling module to keep the main
//! source file under the CLAUDE.md 500-line budget. Covers:
//! - single-threaded time-wheel semantics
//! - `Duration::ZERO` disable path
//! - bucket-rotation TTL behaviour
//! - cross-thread race (exactly one inserter wins)
//! - stress: prune under sustained insertion does not OOM

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::print_stdout)]

use super::*;
use std::sync::Arc;
use std::thread;

// ============================================================================
// Single-threaded Dedup
// ============================================================================

#[test]
fn test_dedup_disabled() {
    let dedup = Dedup::new(Duration::ZERO);

    let data = b"test_packet";
    assert!(!dedup.is_duplicate(data));
    assert!(!dedup.is_duplicate(data)); // Still not duplicate when disabled
}

#[test]
fn test_dedup_rotation_boundary() {
    let mut dedup = Dedup::new(Duration::from_millis(200));

    let data1 = b"packet_1";
    let data2 = b"packet_2";

    assert!(!dedup.check_and_insert(data1));
    assert!(dedup.check_and_insert(data1)); // Duplicate

    dedup.rotate_bucket();
    assert!(dedup.is_duplicate(data1)); // Still in old bucket

    assert!(!dedup.check_and_insert(data2));

    dedup.rotate_bucket();
    assert!(!dedup.is_duplicate(data1)); // Expired
    assert!(dedup.is_duplicate(data2)); // Still valid
}

#[test]
fn test_dedup_different_data_same_length() {
    let mut dedup = Dedup::new(Duration::from_millis(1000));

    // Same length, different content — must not hash-collide under ahash.
    let data1 = b"aaaaaaaa";
    let data2 = b"bbbbbbbb";

    assert!(!dedup.check_and_insert(data1));
    assert!(!dedup.check_and_insert(data2));
}

#[test]
fn test_dedup_time_wheel() {
    let dedup_period = Duration::from_millis(500);
    let bucket_interval = Duration::from_millis(100);
    let mut dedup = Dedup::new(dedup_period);
    assert_eq!(dedup.rotation_interval(), bucket_interval);
    assert_eq!(dedup.num_buckets, 5); // 500ms / 100ms

    let data1 = b"hello";
    let data2 = b"world";

    assert!(!dedup.is_duplicate(data1));
    dedup.insert(data1);

    assert!(dedup.is_duplicate(data1));
    assert!(!dedup.is_duplicate(data2));

    for i in 0..dedup.num_buckets {
        dedup.rotate_bucket();
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

    // Simple expiry pass.
    let dedup_period = Duration::from_millis(200);
    let mut dedup_exp = Dedup::new(dedup_period);
    assert_eq!(dedup_exp.num_buckets, 2);

    let data = b"expire_test";
    assert!(!dedup_exp.is_duplicate(data));
    dedup_exp.insert(data);
    assert!(dedup_exp.is_duplicate(data));

    thread::sleep(dedup_exp.rotation_interval());
    dedup_exp.rotate_bucket();
    assert!(dedup_exp.is_duplicate(data));

    thread::sleep(dedup_exp.rotation_interval());
    dedup_exp.rotate_bucket();
    assert!(!dedup_exp.is_duplicate(data));

    // Zero-period disables entirely.
    let mut dedup_zero = Dedup::new(Duration::ZERO);
    assert!(!dedup_zero.is_duplicate(data1));
    dedup_zero.insert(data1);
    assert!(!dedup_zero.is_duplicate(data1));
}

// ============================================================================
// ConcurrentDedup
// ============================================================================

#[test]
fn test_concurrent_dedup_basic() {
    let cd = ConcurrentDedup::new(Duration::from_millis(500));
    let msg = b"hello_concurrent";

    assert!(!cd.check_and_insert(msg));
    assert!(cd.check_and_insert(msg));

    let msg2 = b"different_message";
    assert!(!cd.check_and_insert(msg2));
}

#[test]
fn test_concurrent_dedup_disabled_when_zero_period() {
    let cd = ConcurrentDedup::new(Duration::ZERO);
    let msg = b"zero_period_msg";

    assert!(!cd.check_and_insert(msg));
    assert!(!cd.check_and_insert(msg));
    assert!(!cd.check_and_insert(msg));
}

#[test]
fn test_concurrent_dedup_rotate_buckets() {
    let cd = ConcurrentDedup::new(Duration::from_millis(200));
    let msg = b"rotate_test";

    assert!(!cd.check_and_insert(msg));
    assert!(cd.check_and_insert(msg));

    cd.rotate_buckets();
    assert!(cd.check_and_insert(msg));

    cd.rotate_buckets();
    assert!(!cd.check_and_insert(msg));
}

#[test]
fn test_concurrent_dedup_cross_thread() {
    let cd = ConcurrentDedup::new(Duration::from_secs(5));
    let cd_arc = Arc::new(cd);
    let msg = b"cross_thread_msg";

    let mut handles = Vec::new();
    for _ in 0..8 {
        let cd_clone = Arc::clone(&cd_arc);
        let m = msg.to_vec();
        handles.push(thread::spawn(move || cd_clone.check_and_insert(&m)));
    }

    let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let non_dup_count = results.iter().filter(|&&r| !r).count();
    assert_eq!(
        non_dup_count, 1,
        "Exactly one thread should insert the message as new"
    );
}

#[test]
fn test_concurrent_dedup_different_messages_not_duplicate() {
    let cd = ConcurrentDedup::new(Duration::from_secs(1));
    for i in 0..20u8 {
        let msg = [i; 4];
        assert!(
            !cd.check_and_insert(&msg),
            "Message {i} should not be a duplicate on first insert"
        );
    }
}

#[test]
fn test_concurrent_dedup_rotation_interval() {
    let cd = ConcurrentDedup::new(Duration::from_millis(500));
    assert_eq!(cd.rotation_interval(), Duration::from_millis(100));

    let cd_zero = ConcurrentDedup::new(Duration::ZERO);
    assert_eq!(cd_zero.rotation_interval(), Duration::ZERO);
}

#[test]
fn test_concurrent_dedup_contention() {
    let dedup = Arc::new(ConcurrentDedup::new(Duration::from_millis(500)));
    let mut handles = vec![];

    for thread_id in 0..16 {
        let dedup_clone = dedup.clone();
        handles.push(thread::spawn(move || {
            let mut duplicates = 0;
            for i in 0..10_000 {
                let shared_payload = format!("shared_packet_{}", i % 100);
                if dedup_clone.check_and_insert(shared_payload.as_bytes()) {
                    duplicates += 1;
                }
                let unique_payload = format!("thread_{}_packet_{}", thread_id, i);
                dedup_clone.check_and_insert(unique_payload.as_bytes());
            }
            duplicates
        }));
    }

    let total_duplicates: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert!(total_duplicates > 0);
}

// ============================================================================
// Stress — bounded memory under sustained insertion
// ============================================================================

fn stress_iterations() -> usize {
    if let Ok(s) = std::env::var("CI_STRESS_ITERATIONS") {
        return s.parse().expect("CI_STRESS_ITERATIONS must be a number");
    }
    let cpus = num_cpus::get();
    if cpus >= 64 {
        10_000_000
    } else if cpus >= 8 {
        1_000_000
    } else if cpus >= 4 {
        500_000
    } else {
        100_000
    }
}

#[test]
fn test_dedup_memory_actually_bounded() {
    let mut dedup = Dedup::new(Duration::from_millis(100));
    let iterations = stress_iterations();

    for i in 0..iterations {
        let data = format!("packet_{}", i);
        dedup.check_and_insert(data.as_bytes());
    }

    std::thread::sleep(Duration::from_millis(150));

    // Insert another batch — should not OOM and should prune old entries.
    for i in iterations..(iterations * 2) {
        let data = format!("packet_{}", i);
        dedup.check_and_insert(data.as_bytes());
    }
    // Reaching here without OOM is the assertion.
}
