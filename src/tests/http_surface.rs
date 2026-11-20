//! What every route shares: protective headers and broken bodies.

use super::*;

#[tokio::test]
async fn a_body_that_breaks_off_is_400() {
    let broken = futures::stream::iter([
        Ok(b"{\"dsn\":\"http://key-a@h/1\"}\n".to_vec()),
        Err(std::io::Error::other("connection reset")),
    ]);
    let r = server()
        .send(post("/api/1/envelope/", &[], Body::from_stream(broken)))
        .await;
    assert_eq!(r.status, 400);
}

#[tokio::test]
async fn every_response_forbids_sniffing_framing_and_caching() {
    let s = server();
    let csp = "default-src 'none'; frame-ancestors 'none'";
    for (req, status) in [
        (post("/api/1/store/", &[], "{}"), 401),
        (request("GET", "/admin"), 200),
        (request("GET", "/nowhere"), 404),
    ] {
        let r = s.send(req).await;
        let h = |name| r.headers[name].to_str().unwrap();
        let got = (
            r.status,
            h("x-content-type-options"),
            h("content-security-policy"),
        );
        assert_eq!(got, (status, "nosniff", csp));
        assert_eq!(h("cache-control"), "no-store");
    }
}
