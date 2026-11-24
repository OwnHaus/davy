use super::*;
#[test]
fn authentication_precedence() {
    let mut headers = HeaderMap::new();
    let dsn = Some("https://dsn-key@example.com/1");
    assert_eq!(key(&headers, None, dsn).as_deref(), Some("dsn-key"));
    assert_eq!(
        key(&headers, Some("sentry_key=query-key"), dsn).as_deref(),
        Some("query-key")
    );
    headers.insert(
        "x-sentry-auth",
        "Sentry sentry_version=7, sentry_key=header-key"
            .parse()
            .unwrap(),
    );
    assert_eq!(
        key(&headers, Some("sentry_key=query-key"), dsn).as_deref(),
        Some("header-key")
    );
}
