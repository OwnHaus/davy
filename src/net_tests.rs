use super::net::{Client, Reach};
use reqwest::Method;
use std::net::IpAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const REFUSED: [&str; 42] = [
    "0.0.0.0",
    "0.1.2.3",
    "10.0.0.1",
    "100.64.0.1",
    "100.127.255.254",
    "127.0.0.1",
    "127.255.255.255",
    "169.254.169.254",
    "172.16.0.1",
    "172.31.255.255",
    "192.0.0.8",
    "192.0.2.1",
    "192.168.1.1",
    "198.18.0.1",
    "198.19.255.255",
    "198.51.100.7",
    "203.0.113.9",
    "224.0.0.1",
    "239.255.255.250",
    "240.0.0.1",
    "255.255.255.255",
    "::",
    "::1",
    "::127.0.0.1",
    "::ffff:127.0.0.1",
    "::ffff:10.0.0.1",
    "::ffff:169.254.169.254",
    "64:ff9b::7f00:1",
    "64:ff9b::a9fe:a9fe",
    "64:ff9b:1::808:808",
    "2002:7f00:1::1",
    "2002:c0a8:101::",
    "100::1",
    "2001::1",
    "2001:db8::1",
    "3fff::1",
    "5f00::1",
    "fc00::1",
    "fd12:3456::1",
    "fe80::1",
    "fec0::1",
    "ff02::1",
];

const PUBLIC: [&str; 20] = [
    "1.1.1.1",
    "8.8.8.8",
    "9.255.255.255",
    "11.0.0.0",
    "100.63.255.255",
    "100.128.0.0",
    "172.15.255.255",
    "172.32.0.0",
    "192.0.1.1",
    "192.169.0.1",
    "198.17.255.255",
    "198.20.0.0",
    "223.255.255.255",
    "::ffff:8.8.8.8",
    "64:ff9b::808:808",
    "2002:808:808::1",
    "2001:4860:4860::8888",
    "2606:4700:4700::1111",
    "2a00:1450:4001::1",
    "3ffe::1",
];

#[test]
fn public_reach_refuses_every_address_that_is_not_public() {
    let judged = |ips: &[&str]| -> Vec<(String, bool)> {
        ips.iter()
            .map(|ip| {
                (
                    ip.to_string(),
                    Reach::Public.allows(ip.parse::<IpAddr>().unwrap()),
                )
            })
            .collect()
    };
    let expect = |ips: &[&str], allowed| -> Vec<(String, bool)> {
        ips.iter().map(|ip| (ip.to_string(), allowed)).collect()
    };
    assert_eq!(judged(&REFUSED), expect(&REFUSED, false));
    assert_eq!(judged(&PUBLIC), expect(&PUBLIC, true));
}

#[test]
fn any_reach_allows_every_address() {
    for ip in ["127.0.0.1", "::1", "10.0.0.1", "169.254.169.254", "8.8.8.8"] {
        assert!(Reach::Any.allows(ip.parse().unwrap()), "{ip}");
    }
}

/// Reads one request without a body from `conn`. False when the peer
/// closed first.
async fn read_request(conn: &mut tokio::net::TcpStream) -> bool {
    let mut seen = Vec::new();
    let mut buf = [0; 1024];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        match conn.read(&mut buf).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
        }
    }
    true
}

/// A server that answers the first request on each connection with
/// keep-alive and closes the connection when the next request on it comes,
/// as a server does whose idle timeout ends as a request arrives.
async fn closes_idle_connections() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                if read_request(&mut conn).await {
                    let ok = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
                    conn.write_all(ok).await.unwrap();
                    read_request(&mut conn).await;
                }
            });
        }
    });
    base
}

#[tokio::test]
async fn each_request_opens_a_new_connection_so_a_closed_idle_one_cannot_fail_it() {
    let base = closes_idle_connections().await;
    let client = Client::new(Reach::Any, 0).unwrap();
    let mut statuses = Vec::new();
    for _ in 0..3 {
        let sent = client.request(Method::GET, &base).unwrap().send().await;
        statuses.push(
            sent.map(|r| r.status().as_u16())
                .map_err(super::net::error_text),
        );
    }
    assert_eq!(statuses, [Ok(200), Ok(200), Ok(200)]);
}
