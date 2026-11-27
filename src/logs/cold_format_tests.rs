use super::{
    Entry, Footer, HEAD, HEADER, Header, MAX_BLOCK_ROWS, Meta, Reader, put_varint, unzigzag, zigzag,
};

fn header(rows: u32, raw: u32) -> Header {
    Header {
        rows,
        raw,
        min_ts: 10,
        max_ts: 20,
        levels: 0b100,
    }
}

fn decoded(h: &Header) -> anyhow::Result<Header> {
    let mut bytes = Vec::new();
    h.encode(&mut bytes);
    assert_eq!(bytes.len(), HEADER);
    Header::decode(&mut Reader::new(&bytes))
}

#[test]
fn varints_round_trip_and_take_leb128_bytes() {
    let mut out = Vec::new();
    put_varint(&mut out, 300);
    assert_eq!(out, [0xac, 0x02]);
    for v in [0, 1, 127, 128, 16_383, 16_384, u64::MAX] {
        let mut out = Vec::new();
        put_varint(&mut out, v);
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), v);
        assert_eq!(r.left(), 0);
    }
    let mut max = Vec::new();
    put_varint(&mut max, u64::MAX);
    assert_eq!(max.len(), 10);
}

#[test]
fn a_varint_past_64_bits_or_the_end_is_refused() {
    let eleven = [0xff; 11];
    assert!(Reader::new(&eleven).varint().is_err());
    let mut over = vec![0xff; 9];
    over.push(0x02);
    assert!(Reader::new(&over).varint().is_err());
    assert!(Reader::new(&[0x80]).varint().is_err());
}

#[test]
fn zigzag_maps_small_numbers_of_both_signs_to_small_ones() {
    assert_eq!(zigzag(0), 0);
    assert_eq!(zigzag(-1), 1);
    assert_eq!(zigzag(1), 2);
    assert_eq!(zigzag(i64::MIN), u64::MAX);
    assert_eq!(unzigzag(3), -2);
    for v in [i64::MIN, -5, 0, 7, i64::MAX] {
        assert_eq!(unzigzag(zigzag(v)), v);
    }
}

#[test]
fn a_header_round_trips_and_a_crafted_one_is_refused() {
    assert_eq!(decoded(&header(3, 30)).unwrap(), header(3, 30));
    assert!(decoded(&header(0, 30)).is_err(), "no rows");
    assert!(
        decoded(&header(MAX_BLOCK_ROWS + 1, 16 << 20)).is_err(),
        "too many rows"
    );
    assert!(decoded(&header(4, 39)).is_err(), "rows that cannot fit");
    assert!(decoded(&header(1, (16 << 20) + 1)).is_err(), "too large");
    let backwards = Header {
        min_ts: 21,
        ..header(1, 10)
    };
    assert!(decoded(&backwards).is_err());
    for levels in [0, 64] {
        assert!(
            decoded(&Header {
                levels,
                ..header(1, 10)
            })
            .is_err()
        );
    }
}

fn footer() -> Footer {
    Footer {
        meta: Meta {
            day: 20_261_120,
            project: 7,
            seq: 2,
            values: true,
            pattern: false,
            level: 9,
        },
        dict: (0, 0),
        dict_checksum: 0,
        rows: 5,
        min_ts: 10,
        max_ts: 20,
        blocks: vec![
            Entry {
                offset: HEAD,
                len: 100,
                checksum: 1,
                header: header(3, 30),
            },
            Entry {
                offset: HEAD + 100,
                len: 50,
                checksum: 2,
                header: header(2, 20),
            },
        ],
    }
}

#[test]
fn a_footer_round_trips() {
    let bytes = footer().encode();
    assert_eq!(bytes.len(), 66 + 2 * 45 + 8);
    let back = Footer::decode(&bytes, HEAD + 150).unwrap();
    assert_eq!(back.meta, footer().meta);
    assert_eq!(back.blocks.len(), 2);
    assert_eq!(back.blocks[1].offset, 112);
    assert_eq!(back.rows, 5);
}

#[test]
fn a_footer_with_a_flipped_bit_or_wrong_places_is_refused() {
    let bytes = footer().encode();
    for i in [0, 20, 70, bytes.len() - 1] {
        let mut bad = bytes.clone();
        bad[i] ^= 1;
        assert!(Footer::decode(&bad, HEAD + 150).is_err(), "byte {i}");
    }
    assert!(
        Footer::decode(&bytes, HEAD + 149).is_err(),
        "a block past the end"
    );
    assert!(
        Footer::decode(&bytes, HEAD + 151).is_err(),
        "a gap before the end"
    );
    assert!(Footer::decode(&bytes[..10], HEAD + 150).is_err());
    let mut rows = footer();
    rows.rows = 6;
    assert!(Footer::decode(&rows.encode(), HEAD + 150).is_err());
    let mut gap = footer();
    gap.blocks[1].offset += 1;
    assert!(Footer::decode(&gap.encode(), HEAD + 151).is_err());
    let mut big = footer();
    big.blocks[1].len = 17 << 20;
    let end = HEAD + 100 + (17 << 20);
    assert!(
        Footer::decode(&big.encode(), end).is_err(),
        "a block too big to read"
    );
    let mut dict = footer();
    dict.dict = (HEAD + 150, 2 << 20);
    assert!(Footer::decode(&dict.encode(), HEAD + 150 + (2 << 20)).is_err());
}
