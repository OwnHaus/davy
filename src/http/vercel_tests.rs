use super::*;
use crate::http::body::Budget;

/// RFC 2202, test case 2 for HMAC-SHA1.
const KEY: &str = "Jefe";
const DATA: &[u8] = b"what do ya want for nothing?";
const MAC: &str = "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79";

fn headers(signature: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(s) = signature {
        h.insert(SIGNATURE, s.parse().unwrap());
    }
    h
}

fn signed(signature: &str) -> Signed {
    Signed::demand(Some(KEY.to_owned()), &headers(Some(signature)))
        .unwrap()
        .unwrap()
}

fn gzip(body: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body).unwrap();
    gz.finish().unwrap()
}

#[test]
fn the_signature_is_the_hmac_sha1_of_the_body() {
    assert!(signed(MAC).matches(DATA));
    assert!(signed(&MAC.to_ascii_uppercase()).matches(DATA));
    assert!(!signed(MAC).matches(b"what do ya want for nothing!"));
    let mut other = MAC.to_owned();
    other.replace_range(39.., "8");
    assert!(!signed(&other).matches(DATA));
}

#[test]
fn a_project_without_a_secret_needs_no_signature() {
    assert!(Signed::demand(None, &headers(None)).unwrap().is_none());
    assert!(Signed::demand(None, &headers(Some("x"))).unwrap().is_none());
}

#[test]
fn a_missing_or_malformed_signature_is_403() {
    let refused = |signature: Option<&str>| {
        Signed::demand(Some(KEY.to_owned()), &headers(signature))
            .err()
            .map(|(status, why)| (status.as_u16(), why))
    };
    assert_eq!(
        refused(None),
        Some((403, "missing x-vercel-signature".to_owned()))
    );
    for bad in [
        "",
        "effcdf6ae5eb2fa2d27416d5f184df9c259a7c7",
        "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79a",
        "effcdf6ae5eb2fa2d27416d5f184df9c259a7c7g",
        "+ffcdf6ae5eb2fa2d27416d5f184df9c259a7c79",
        "sha1=effcdf6ae5eb2fa2d27416d5f184df9c259a7c79",
    ] {
        assert_eq!(
            refused(Some(bad)),
            Some((403, "invalid x-vercel-signature".to_owned())),
            "{bad:?}"
        );
    }
}

fn hmac(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(body);
    crate::text::hex(&mac.finalize().into_bytes())
}

#[test]
fn the_signature_covers_the_body_as_sent() {
    let budget = Budget::new(32 << 20);
    let decode = |signature: &str, bytes: Vec<u8>, encoding: &str| {
        decode(Some(signed(signature)), bytes, encoding, &mut budget.hold())
            .map_err(|(status, why)| (status.as_u16(), why))
    };
    let refused = Err((403, "invalid x-vercel-signature".to_owned()));
    assert_eq!(hmac(DATA), MAC);
    assert_eq!(decode(MAC, DATA.to_vec(), ""), Ok(DATA.to_vec()));
    assert_eq!(decode(MAC, DATA.to_vec(), "identity"), Ok(DATA.to_vec()));
    let wire = gzip(DATA);
    assert_eq!(decode(&hmac(&wire), wire, "gzip"), Ok(DATA.to_vec()));
    assert_eq!(decode(MAC, gzip(DATA), "gzip"), refused);
    assert_eq!(decode(MAC, b"other".to_vec(), ""), refused);
    let not_gzip = b"not gzip".to_vec();
    assert_eq!(decode(MAC, not_gzip.clone(), "gzip"), refused);
    assert_eq!(
        decode(&hmac(&not_gzip), not_gzip, "gzip").map_err(|(status, _)| status),
        Err(400)
    );
}

#[test]
fn a_body_without_a_signature_to_check_is_only_decoded() {
    let budget = Budget::new(32 << 20);
    let body = decode(None, gzip(DATA), "gzip", &mut budget.hold()).unwrap();
    assert_eq!(body, DATA);
}
