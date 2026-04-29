#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use crate::endpoint_core::ExponentialBackoff;
use std::time::Duration;

#[test]
fn test_exponential_backoff_initial() {
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
    assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
}

#[test]
fn test_exponential_backoff_doubles() {
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
    assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(2));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(4));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(8));
}

#[test]
fn test_exponential_backoff_caps_at_max() {
    let mut backoff =
        ExponentialBackoff::new(Duration::from_secs(10), Duration::from_secs(30), 2.0);
    assert_eq!(backoff.next_backoff(), Duration::from_secs(10));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(20));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(30));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(30));
}

#[test]
fn test_exponential_backoff_reset() {
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
    backoff.next_backoff();
    backoff.next_backoff();
    backoff.next_backoff();
    backoff.reset();
    assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
}

#[test]
fn test_exponential_backoff_custom_multiplier() {
    let mut backoff =
        ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(100), 3.0);
    assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(3));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(9));
    assert_eq!(backoff.next_backoff(), Duration::from_secs(27));
}
