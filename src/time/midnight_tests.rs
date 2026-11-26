//! The next midnight UTC and the seconds to it, which a quota's
//! `Retry-After` and `resets_at` use.

use super::*;

#[test]
fn retry_counts_whole_seconds_to_the_next_midnight() {
    let midnight = 1_795_132_800_000_000_000;
    assert_eq!(next_midnight(midnight), 1_795_219_200_000_000_000);
    assert_eq!(next_midnight(midnight - 1), midnight);
    assert_eq!(next_midnight(-1), 0);
    assert_eq!(retry_secs(midnight - 1, midnight), 1);
    assert_eq!(retry_secs(midnight - 1_500_000_000, midnight), 2);
    assert_eq!(retry_secs(midnight, midnight), 1);
    assert_eq!(retry_secs(midnight + 5, midnight), 1);
    assert_eq!(retry_secs(midnight, next_midnight(midnight)), 86_400);
}
