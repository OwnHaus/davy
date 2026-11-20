use super::*;
use std::os::unix::net::UnixDatagram;

#[test]
fn send_writes_the_message_to_a_socket_path() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "notify-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let systemd = UnixDatagram::bind(&path).unwrap();
    send(path.as_os_str(), "READY=1").unwrap();
    let mut buf = [0_u8; 16];
    let n = systemd.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"READY=1");
    std::fs::remove_file(&path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn send_reaches_an_abstract_socket() {
    use std::os::linux::net::SocketAddrExt as _;
    let name = format!("davy-test-{}", uuid::Uuid::new_v4().simple());
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let systemd = UnixDatagram::bind_addr(&addr).unwrap();
    send(OsStr::new(&format!("@{name}")), "READY=1").unwrap();
    let mut buf = [0_u8; 16];
    let n = systemd.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"READY=1");
}

#[test]
fn send_reports_a_missing_socket() {
    assert!(send(OsStr::new("/nonexistent/davy-notify.sock"), "READY=1").is_err());
}
