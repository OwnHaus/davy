use super::*;

#[test]
fn a_msgpack_body_with_bytes_after_its_value_is_refused() {
    let mut body = Vec::new();
    let record = rmpv::Value::Map(vec![("message".into(), "x".into())]);
    rmpv::encode::write_value(&mut body, &record).unwrap();
    assert_eq!(
        parse(&body, "application/msgpack", 1, 5).unwrap()[0].message,
        "x"
    );
    body.push(0xc0);
    assert_eq!(
        parse(&body, "application/msgpack", 1, 5).unwrap_err(),
        "bytes after the msgpack value"
    );
}

#[test]
fn a_msgpack_timestamp_needs_fewer_than_1e9_nanoseconds() {
    let secs: u64 = 1_795_161_600;
    let packed = |nanos: u64| ((nanos << 34) | secs).to_be_bytes();
    let wide = |nanos: u32| {
        [
            nanos.to_be_bytes().as_slice(),
            &1_795_161_600i64.to_be_bytes(),
        ]
        .concat()
    };
    assert_eq!(
        msgpack_timestamp(&packed(999_999_999)),
        Some(1_795_161_600_999_999_999)
    );
    assert_eq!(msgpack_timestamp(&packed(1_000_000_000)), None);
    assert_eq!(
        msgpack_timestamp(&wide(999_999_999)),
        Some(1_795_161_600_999_999_999)
    );
    assert_eq!(msgpack_timestamp(&wide(1_000_000_000)), None);
}

#[test]
fn msgpack_timestamp_ext() {
    assert_eq!(
        msgpack_timestamp(&1_795_161_600u32.to_be_bytes()),
        Some(1_795_161_600_000_000_000)
    );
    let secs: u64 = 1_795_161_600;
    let packed = (500 << 34) | secs;
    assert_eq!(
        msgpack_timestamp(&packed.to_be_bytes()),
        Some(1_795_161_600_000_000_500)
    );
    let mut wide = 7u32.to_be_bytes().to_vec();
    wide.extend(1_795_161_600i64.to_be_bytes());
    assert_eq!(msgpack_timestamp(&wide), Some(1_795_161_600_000_000_007));
    assert_eq!(msgpack_timestamp(&[0; 5]), None);

    let mut body = Vec::new();
    rmpv::encode::write_value(
        &mut body,
        &rmpv::Value::Map(vec![
            ("message".into(), "ext".into()),
            (
                "dt".into(),
                rmpv::Value::Ext(-1, 1_795_161_600u32.to_be_bytes().to_vec()),
            ),
        ]),
    )
    .unwrap();
    let records = parse(&body, "application/msgpack", 1, 5).unwrap();
    assert_eq!(records[0].timestamp, 1_795_161_600_000_000_000);
}

#[test]
fn msgpack_nested_deeper_than_its_price_allows_is_refused() {
    // 128 arrays around a nil nest 129 levels, as deep as JSON may.
    let nested = |levels: usize| [vec![0x91; levels - 1], vec![0xc0]].concat();
    let fits = nested(129);
    assert!(cost(&fits, "application/msgpack").is_ok());
    assert_eq!(parse(&fits, "application/msgpack", 1, 0), Ok(vec![]));
    let deep = nested(130);
    assert!(cost(&deep, "application/msgpack").is_err());
    assert!(parse(&deep, "application/msgpack", 1, 0).is_err());
}
