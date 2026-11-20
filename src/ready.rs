//! Tells systemd that the server listens. The unit has `Type=notify`, so
//! `systemctl restart davy` returns only once requests are answered and a
//! `davy doctor` right after it finds the server up.

use anyhow::Context as _;
use std::ffi::OsStr;

/// Sends `READY=1` to the socket systemd names in `NOTIFY_SOCKET`. Under
/// launchd or in a shell the variable is unset and there is nobody to tell.
pub fn notify() -> anyhow::Result<()> {
    match std::env::var_os("NOTIFY_SOCKET") {
        Some(socket) => send(&socket, "READY=1").with_context(|| {
            format!(
                "cannot tell systemd at {} that the server is ready",
                socket.display()
            )
        }),
        None => Ok(()),
    }
}

/// systemd passes a path, or on Linux an abstract name that starts with `@`.
#[cfg(unix)]
fn send(socket: &OsStr, message: &str) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let addr = match socket.as_bytes().strip_prefix(b"@") {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Some(name) => <SocketAddr as std::os::linux::net::SocketAddrExt>::from_abstract_name(name)?,
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        Some(_) => anyhow::bail!("abstract sockets exist only on Linux"),
        None => SocketAddr::from_pathname(socket)?,
    };
    UnixDatagram::unbound()?.send_to_addr(message.as_bytes(), &addr)?;
    Ok(())
}

#[cfg(not(unix))]
fn send(_socket: &OsStr, _message: &str) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "ready_tests.rs"]
mod tests;
