#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::arithmetic_side_effects)]

use crate::endpoint_core::timestamp_us_fast;
use std::time::SystemTime;

#[test]
fn test_timestamp_us_fast_monotonic_walltime() {
    let t1 = timestamp_us_fast();
    let t2 = timestamp_us_fast();
    assert!(t2 >= t1);

    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let tolerance = 5_000_000;
    assert!(t2 + tolerance >= now);
    assert!(t2 <= now + tolerance);
}
