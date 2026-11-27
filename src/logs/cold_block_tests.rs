use super::{super::super::hit::Row, ATTRIBUTES, Block, Columns, MESSAGE, SERVICE};

fn row(ts: i64, message: &str) -> Row {
    Row {
        ts,
        observed: ts.wrapping_add(5),
        level: 3,
        source: 1,
        pattern: None,
        service: None,
        trace_id: None,
        span_id: None,
        message: message.to_owned(),
        attributes: String::new(),
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            level: 5,
            pattern: Some(42),
            service: Some("api".into()),
            trace_id: Some(String::new()),
            attributes: r#"{"user":"ü","n":1}"#.into(),
            ..row(1_000, "first")
        },
        row(900, "ŝecond, older"),
        Row {
            level: 0,
            span_id: Some("s1".into()),
            ..row(i64::MAX, "")
        },
    ]
}

fn encoded(rows: &[Row]) -> (super::Header, Vec<u8>) {
    let mut c = Columns::default();
    for r in rows {
        assert!(c.fits(r));
        c.push(r);
    }
    c.take()
}

#[test]
fn rows_round_trip_through_a_block() {
    let (header, bytes) = encoded(&rows());
    assert_eq!(header.rows, 3);
    assert_eq!(header.raw, u32::try_from(bytes.len()).unwrap());
    assert_eq!((header.min_ts, header.max_ts), (900, i64::MAX));
    assert_eq!(header.levels, 0b10100);
    let block = Block::decode(bytes, 3).unwrap();
    assert_eq!(block.len(), 3);
    assert_eq!(block.ts, [1_000, 900, i64::MAX]);
    for (i, r) in rows().iter().enumerate() {
        assert_eq!(&block.row(i).unwrap(), r);
    }
    assert_eq!(block.raw(MESSAGE, 1), "ŝecond, older".as_bytes());
    assert_eq!(block.raw(ATTRIBUTES, 0), r#"{"user":"ü","n":1}"#.as_bytes());
    assert_eq!(block.optional(SERVICE, 1).unwrap(), None);
}

#[test]
fn a_block_without_a_known_level_counts_as_info() {
    let (header, _) = encoded(&[Row {
        level: 0,
        ..row(1, "x")
    }]);
    assert_eq!(header.levels, 0b100);
}

#[test]
fn a_block_with_other_bytes_than_its_rows_is_refused() {
    let (_, bytes) = encoded(&rows());
    assert!(Block::decode(bytes.clone(), 4).is_err(), "more rows");
    assert!(Block::decode(bytes.clone(), 2).is_err(), "fewer rows");
    assert!(Block::decode(bytes[..bytes.len() - 1].to_vec(), 3).is_err());
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(Block::decode(longer, 3).is_err());
    let (_, one) = encoded(&[row(1, "x")]);
    // ts, observed, level, source, then the pattern flag.
    let mut flag = one.clone();
    flag[4] = 2;
    assert!(Block::decode(flag, 1).is_err());
    let mut length = one;
    // The message's length, after the three optional ones.
    length[8] = 100;
    assert!(Block::decode(length, 1).is_err());
}

#[test]
fn text_that_is_not_utf8_fails_when_read_not_when_decoded() {
    let (_, mut bytes) = encoded(&[row(1, "ab")]);
    let at = bytes.len() - 2;
    bytes[at] = 0xff;
    let block = Block::decode(bytes, 1).unwrap();
    assert!(block.row(0).is_err());
    assert_eq!(block.raw(MESSAGE, 0), [0xff, b'b']);
}
