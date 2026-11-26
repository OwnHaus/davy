use super::*;

#[test]
fn a_quota_names_each_refused_category_until_midnight_or_five_minutes() {
    let none = Limits {
        quota: [false, false],
        midnight: 3600,
    };
    assert_eq!(Limited::of(&none), None);
    let both = Limits {
        quota: [true, true],
        midnight: 3600,
    };
    let mut h = HeaderMap::new();
    Limited::of(&both).unwrap().set(&mut h, true);
    assert_eq!(
        h[RATE_LIMITS],
        "300:error:project:davy_quota, 300:log_item:project:davy_quota"
    );
    assert_eq!(h[header::RETRY_AFTER], "300");
    let logs = Limits {
        quota: [false, true],
        midnight: 60,
    };
    let mut h = HeaderMap::new();
    Limited::of(&logs).unwrap().set(&mut h, false);
    assert_eq!(h[RATE_LIMITS], "60:log_item:project:davy_quota");
    assert!(h.get(header::RETRY_AFTER).is_none());
}

#[test]
fn merge_puts_a_handlers_entries_after_the_fixed_ones() {
    let mut h = HeaderMap::new();
    merge(&mut h, false);
    assert_eq!(
        h[RATE_LIMITS],
        "86400:transaction;span;replay;profile;profile_chunk:organization"
    );
    assert!(h.get(header::RETRY_AFTER).is_none());
    let mut h = HeaderMap::new();
    h.insert(
        RATE_LIMITS,
        HeaderValue::from_static("60:error:project:davy_quota"),
    );
    h.insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    merge(&mut h, true);
    assert_eq!(
        h[RATE_LIMITS],
        "86400:transaction;span;replay;profile;profile_chunk:organization, 60:error:project:davy_quota"
    );
    assert_eq!(h[header::RETRY_AFTER], "60");
    let mut h = HeaderMap::new();
    merge(&mut h, true);
    assert_eq!(h[header::RETRY_AFTER], "1");
}
