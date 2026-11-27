use super::{
    super::{
        super::hit::Row,
        format::{Entry, Footer, HEAD, HEADER, Header, Meta, checksum, head},
    },
    ColdFile, Params, Writer,
};
use crate::test_support::scratch;
use std::path::{Path, PathBuf};

fn row(i: i64) -> Row {
    Row {
        ts: 1_000 + i,
        observed: 1_000 + i,
        level: 3,
        source: 1,
        pattern: Some(7),
        service: Some("api".into()),
        trace_id: None,
        span_id: None,
        message: format!("request {i} completed in {} ms", i % 97),
        attributes: format!(r#"{{"n":{i}}}"#),
    }
}

fn meta() -> Meta {
    Meta {
        day: 20_261_120,
        project: 3,
        seq: 1,
        values: true,
        pattern: true,
        level: 3,
    }
}

/// A file of `n` rows in blocks of about `block_bytes`.
fn write(dir: &Path, n: i64, block_bytes: usize, dict: Option<Vec<u8>>) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("3-1.dlc");
    let params = Params {
        level: 3,
        block_bytes,
        dict,
    };
    let mut w = Writer::create(&path, params).unwrap();
    for i in 0..n {
        w.push(&row(i)).unwrap();
    }
    let size = w.finish(meta()).unwrap();
    assert_eq!(size, std::fs::metadata(&path).unwrap().len());
    path
}

fn all_rows(f: &mut ColdFile) -> Vec<Row> {
    let mut rows = Vec::new();
    for i in 0..f.footer.blocks.len() {
        let block = f.block(i).unwrap();
        rows.extend((0..block.len()).map(|j| block.row(j).unwrap()));
    }
    rows
}

#[test]
fn a_file_round_trips_its_rows_block_by_block() {
    let dir = scratch("cold-file");
    let path = write(&dir, 3_000, 16 << 10, None);
    let mut f = ColdFile::open(&path).unwrap();
    assert_eq!(f.footer.meta, meta());
    assert_eq!(f.footer.rows, 3_000);
    assert_eq!((f.footer.min_ts, f.footer.max_ts), (1_000, 3_999));
    assert!(
        f.footer.blocks.len() > 3,
        "{} blocks",
        f.footer.blocks.len()
    );
    assert_eq!(all_rows(&mut f), (0..3_000).map(row).collect::<Vec<_>>());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_with_a_dictionary_round_trips() {
    let dir = scratch("cold-dict");
    let samples: Vec<Vec<u8>> = (0..2_000).map(|i| row(i).message.into_bytes()).collect();
    let dict = zstd::dict::from_samples(&samples, 4_096).unwrap();
    let path = write(&dir, 500, 4 << 10, Some(dict));
    let mut f = ColdFile::open(&path).unwrap();
    assert!(f.footer.dict.1 > 0);
    assert_eq!(all_rows(&mut f), (0..500).map(row).collect::<Vec<_>>());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_corrupt_block_fails_alone_and_a_cut_file_fails_to_open() {
    let dir = scratch("cold-corrupt");
    let path = write(&dir, 3_000, 16 << 10, None);
    let mut bytes = std::fs::read(&path).unwrap();
    let second = ColdFile::open(&path).unwrap().footer.blocks[1];
    let at = usize::try_from(second.offset).unwrap() + 40;
    bytes[at] ^= 0x10;
    let bad = dir.join("bad.dlc");
    std::fs::write(&bad, &bytes).unwrap();
    let mut f = ColdFile::open(&bad).unwrap();
    assert!(f.block(0).is_ok());
    let e = f.block(1).err().unwrap().to_string();
    assert_eq!(e, "block 1's checksum does not match");
    assert!(f.block(2).is_ok());
    let original = std::fs::read(&path).unwrap();
    for len in [0, 11, 27, original.len() / 2, original.len() - 1] {
        let cut = dir.join(format!("cut-{len}.dlc"));
        std::fs::write(&cut, &original[..len]).unwrap();
        assert!(ColdFile::open(&cut).is_err(), "cut at {len}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_symlink_or_a_directory_is_not_read() {
    let dir = scratch("cold-link");
    let path = write(&dir, 10, 1 << 20, None);
    let link = dir.join("link.dlc");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(ColdFile::open(&link).is_err());
    assert!(ColdFile::open(&dir).is_err());
    assert!(
        Writer::create(
            &link,
            Params {
                level: 3,
                block_bytes: 1,
                dict: None
            }
        )
        .is_err()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A file whose one block holds `frame` and claims `header`.
fn crafted(path: &Path, header: Header, frame: &[u8]) {
    let mut bytes = head().to_vec();
    let mut block = Vec::new();
    header.encode(&mut block);
    block.extend_from_slice(frame);
    let entry = Entry {
        offset: HEAD,
        len: u32::try_from(block.len()).unwrap(),
        checksum: checksum(frame),
        header,
    };
    bytes.extend_from_slice(&block);
    let footer = Footer {
        meta: meta(),
        dict: (0, 0),
        dict_checksum: 0,
        rows: u64::from(header.rows),
        min_ts: 1,
        max_ts: 1,
        blocks: vec![entry],
    }
    .encode();
    bytes.extend_from_slice(&footer);
    bytes.extend_from_slice(&head());
    bytes.extend_from_slice(&u32::try_from(footer.len()).unwrap().to_le_bytes());
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_frame_that_decompresses_past_its_claim_is_refused() {
    let dir = scratch("cold-bomb");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bomb.dlc");
    let bomb = zstd::bulk::compress(&vec![0; 64 << 20], 19).unwrap();
    assert!(bomb.len() < 8 << 10, "{} bytes", bomb.len());
    let header = Header {
        rows: 1,
        raw: 100,
        min_ts: 1,
        max_ts: 1,
        levels: 0b100,
    };
    crafted(&path, header, &bomb);
    let mut f = ColdFile::open(&path).unwrap();
    assert_eq!(
        f.block(0).err().unwrap().to_string(),
        "decompressing block 0"
    );
    let short = zstd::bulk::compress(&[0; 50], 3).unwrap();
    crafted(&path, header, &short);
    let mut f = ColdFile::open(&path).unwrap();
    assert_eq!(
        f.block(0).err().unwrap().to_string(),
        "block 0 holds 50 bytes, its header says 100"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bytes_in_memory_read_as_the_file_and_a_flip_anywhere_never_panics() {
    let dir = scratch("cold-memory");
    let path = write(&dir, 3_000, 16 << 10, None);
    let bytes = std::fs::read(&path).unwrap();
    let read = super::super::bench::read;
    assert_eq!(read(&bytes, None).unwrap(), 3_000);
    assert_eq!(read(&bytes, Some("\"request 5 completed\"")).unwrap(), 1);
    let blocks = ColdFile::open(&path).unwrap().footer.blocks.len();
    assert_eq!(
        ColdFile::read(bytes.as_slice())
            .unwrap()
            .footer
            .blocks
            .len(),
        blocks
    );

    // A flipped bit in a block loses that block's rows and no more; one in
    // the head, footer or trailer refuses the file.
    let second = ColdFile::read(bytes.as_slice()).unwrap().footer.blocks[1];
    let mut flipped = bytes.clone();
    flipped[usize::try_from(second.offset).unwrap() + 40] ^= 1;
    assert_eq!(
        read(&flipped, None).unwrap(),
        3_000 - u64::from(second.header.rows)
    );
    for at in (0..bytes.len()).step_by(97) {
        let mut flipped = bytes.clone();
        flipped[at] ^= 0x80;
        assert!(
            read(&flipped, None).map_or(true, |n| n <= 3_000),
            "flip at {at}"
        );
        assert!(read(&bytes[..at], None).is_err(), "cut at {at}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_block_whose_rows_are_of_other_levels_than_its_header_gives_is_refused() {
    let dir = scratch("cold-levels-lie");
    let path = write(&dir, 10, 1 << 20, None);
    let f = ColdFile::open(&path).unwrap();
    let frame = f.checked(0).unwrap()[HEADER..].to_vec();
    let header = f.footer.blocks[0].header;
    assert_eq!(header.levels, 0b100, "every row is of level 3");
    // A header that adds a level, or gives another, lies.
    for levels in [0b10_0100, 0b10_0000] {
        let lying = dir.join("lying.dlc");
        crafted(&lying, Header { levels, ..header }, &frame);
        let mut f = ColdFile::open(&lying).unwrap();
        assert_eq!(
            f.block(0).err().unwrap().to_string(),
            format!("block 0 holds rows of levels 0b100, its header says {levels:#b}")
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_block_with_a_row_outside_its_header_is_refused() {
    let dir = scratch("cold-header-lies");
    let path = write(&dir, 10, 1 << 20, None);
    let f = ColdFile::open(&path).unwrap();
    assert_eq!(f.footer.blocks.len(), 1);
    let frame = f.checked(0).unwrap()[HEADER..].to_vec();
    // The rows run from 1000 to 1009: a header that ends at 1005 lies.
    let header = Header {
        max_ts: 1_005,
        ..f.footer.blocks[0].header
    };
    let lying = dir.join("lying.dlc");
    crafted(&lying, header, &frame);
    let mut f = ColdFile::open(&lying).unwrap();
    assert_eq!(
        f.block(0).err().unwrap().to_string(),
        "block 0 holds a row outside the times its header gives"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
