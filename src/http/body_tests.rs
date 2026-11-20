use super::*;
use std::io::Write;

const MIB: usize = 1024 * 1024;

fn compress(encoding: &str, payload: &[u8]) -> Vec<u8> {
    match encoding {
        "gzip" => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(payload).unwrap();
            e.finish().unwrap()
        }
        "deflate" => {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(payload).unwrap();
            e.finish().unwrap()
        }
        "br" => {
            let mut out = Vec::new();
            brotli::CompressorWriter::new(&mut out, 4096, 3, 20)
                .write_all(payload)
                .unwrap();
            out
        }
        "zstd" => zstd::stream::encode_all(payload, 1).unwrap(),
        _ => payload.to_vec(),
    }
}

/// Bytes that do not compress, so a prefix of the compressed body holds a
/// prefix of the payload.
fn noise(len: usize) -> Vec<u8> {
    let mut x: u32 = 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn compression_formats() {
    let payload = b"{}\n{\"type\":\"event\"}\n{\"message\":\"test\"}";
    let budget = Budget::new(MEMORY);
    for encoding in ["", "identity", "gzip", "deflate", "br", "zstd"] {
        let body = compress(encoding, payload);
        assert_eq!(decode(body, encoding, &mut budget.hold()).unwrap(), payload);
    }
    for encoding in ["gzip", "deflate", "br", "zstd"] {
        let garbage = b"invalid compressed stream".to_vec();
        assert!(decode(garbage, encoding, &mut budget.hold()).is_err());
    }
    let unknown = decode(b"{}".to_vec(), "lz4", &mut budget.hold()).unwrap_err();
    assert_eq!(unknown.0, StatusCode::BAD_REQUEST);
}

#[test]
fn a_request_over_the_whole_budget_is_413_and_a_busy_budget_is_429() {
    let budget = Budget::new(MIB);
    let mut first = budget.hold();
    first.take(600 * 1024).unwrap();
    let too_large = first.take(500 * 1024).unwrap_err();
    assert_eq!(too_large.0, StatusCode::PAYLOAD_TOO_LARGE);
    let mut second = budget.hold();
    assert_eq!(
        second.take(500 * 1024).unwrap_err().0,
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(first);
    second.take(MIB).unwrap();
}

#[test]
fn memory_no_longer_in_use_goes_back() {
    let budget = Budget::new(MIB);
    let mut held = budget.hold();
    held.take(MIB).unwrap();
    held.keep(600 * 1024);
    held.release(100 * 1024);
    let mut other = budget.hold();
    other.take(500 * 1024).unwrap();
    assert!(budget.hold().take(25 * 1024).is_err());
    budget.hold().take(24 * 1024).unwrap();
}

#[test]
fn decompression_pays_for_every_room_its_buffer_had() {
    let body = compress("gzip", &vec![0; 600 * 1024]);
    // The buffer grows through 64, 128, 256, 512 and 1024 KiB, and holds two
    // of them at a time while it grows, which is more than 1 MiB.
    let refused = decode(body.clone(), "gzip", &mut Budget::new(MIB).hold()).unwrap_err();
    assert_eq!(refused.0, StatusCode::PAYLOAD_TOO_LARGE);
    // The rooms and the decoder's 128 KiB take more than 2 MiB.
    let budget = Budget::new(3 * MIB);
    let mut held = budget.hold();
    assert_eq!(decode(body, "gzip", &mut held).unwrap().len(), 600 * 1024);
    // Only the last room stays paid.
    budget.hold().take(2 * MIB).unwrap();
}

#[test]
fn a_body_decompresses_to_at_most_64_mib() {
    let budget = Budget::new(MEMORY);
    let fits = compress("zstd", &vec![0; DECOMPRESSED]);
    assert_eq!(
        decode(fits, "zstd", &mut budget.hold()).unwrap().len(),
        DECOMPRESSED
    );
    let over = compress("zstd", &vec![0; DECOMPRESSED + 1]);
    let refused = decode(over, "zstd", &mut budget.hold()).unwrap_err();
    assert_eq!(
        refused,
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "decompressed body too large".into()
        )
    );
}

#[test]
fn the_header_line_is_found_in_a_prefix_of_the_compressed_body() {
    let header = b"{\"dsn\":\"https://key@example.com/1\"}\n";
    let payload = [&header[..], &noise(MIB)].concat();
    for encoding in ["identity", "gzip", "deflate", "br", "zstd"] {
        let body = compress(encoding, &payload);
        let line = header_line(&body[..BEFORE_AUTH], encoding, false).unwrap();
        assert_eq!(line.as_deref(), Some(&header[..]), "{encoding}");
        assert_eq!(header_line(&body[..8], encoding, false).unwrap(), None);
    }
}

#[test]
fn a_whole_body_of_one_line_is_its_header() {
    assert_eq!(
        header_line(b"{}", "identity", true).unwrap().as_deref(),
        Some(&b"{}"[..])
    );
    assert!(header_line(b"\x1f\x8b broken", "gzip", true).is_err());
    let long = vec![b' '; MAX_HEADER_LINE + 1];
    assert!(header_line(&long, "identity", false).is_err());
}

fn zstd_with_window(window_log: u32, payload: &[u8]) -> Vec<u8> {
    let mut e = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    e.window_log(window_log).unwrap();
    e.include_contentsize(false).unwrap();
    e.write_all(payload).unwrap();
    e.finish().unwrap()
}

#[test]
fn a_zstd_window_over_8_mib_is_refused() {
    // RFC 9659 lets an HTTP decoder refuse a window over 8 MiB, and a
    // decoder allocates the window a frame asks for before any output.
    let budget = Budget::new(MEMORY);
    let fits = zstd_with_window(23, b"{}\n");
    assert_eq!(decode(fits, "zstd", &mut budget.hold()).unwrap(), b"{}\n");
    let large = zstd_with_window(27, b"{}\n");
    let refused = decode(large.clone(), "zstd", &mut budget.hold()).unwrap_err();
    assert_eq!(refused.0, StatusCode::BAD_REQUEST);
    assert!(header_line(&large, "zstd", true).is_err());
}

fn brotli_with_window(lgwin: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    brotli::CompressorWriter::new(&mut out, 4096, 5, lgwin)
        .write_all(payload)
        .unwrap();
    out
}

#[test]
fn a_large_window_brotli_stream_is_refused() {
    // Large-window brotli is an extension that HTTP's brotli (RFC 7932)
    // does not have, and its window can be 1 GiB.
    let params = brotli::enc::BrotliEncoderParams {
        large_window: true,
        lgwin: 30,
        ..Default::default()
    };
    let mut large = Vec::new();
    brotli::BrotliCompress(&mut &b"{}\n"[..], &mut large, &params).unwrap();
    let refused = decode(large, "br", &mut Budget::new(MEMORY).hold()).unwrap_err();
    assert_eq!(refused.0, StatusCode::BAD_REQUEST);
}

#[test]
fn a_decoder_pays_for_its_window() {
    let budget = Budget::new(8 * MIB);
    let wide = brotli_with_window(24, b"{}\n");
    let refused = decode(wide, "br", &mut budget.hold()).unwrap_err();
    assert_eq!(refused.0, StatusCode::PAYLOAD_TOO_LARGE);
    let narrow = brotli_with_window(16, b"{}\n");
    assert_eq!(decode(narrow, "br", &mut budget.hold()).unwrap(), b"{}\n");
    budget.hold().take(8 * MIB).unwrap();
}

#[tokio::test]
async fn a_decoder_pays_for_its_window_before_auth() {
    let budget = Budget::new(8 * MIB);
    let wide = brotli_with_window(24, b"{\"dsn\":\"https://key@example.com/1\"}\n");
    let refused = read_envelope(Body::from(wide), &mut budget.hold(), "br", |_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(refused.0, StatusCode::PAYLOAD_TOO_LARGE);
    budget.hold().take(8 * MIB).unwrap();
}
