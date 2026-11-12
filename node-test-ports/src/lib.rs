//! Free TCP ports for the nodes a test starts (BL-221), shared by the harnesses that start
//! `rdashd` or `dashd` on a port they pick. A test-only crate (layer T): depend on it only as a
//! dev-dependency.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::SystemTime;

use socket2::{Domain, Socket, Type};

/// A TCP port nothing listens on now, on either loopback address, for a node to bind.
///
/// On Linux it is below the kernel's ephemeral range (`ip_local_port_range`; elsewhere 32768 is
/// assumed), so no outgoing connection or `bind(0)` on the box takes it before the node binds
/// it, which a port from `bind(0)` risked (BL-221). Each process walks the up to 8192 ports
/// (above 1024) below the range from its own starting point, so tests in a process never share a
/// port and parallel test processes rarely meet; when they do, the node that lost the port exits
/// during init and its harness starts it again on the next one. With fewer than 1024 such ports
/// (a range starting below 2048) it falls back to `bind(0)`.
///
/// The probe binds with `SO_REUSEADDR` and never listens. It still meets any listener, and the
/// copy a child spawning on another thread holds until its `exec` blocks no binder that also sets
/// `SO_REUSEADDR` (rdashd's and dashd's listeners, std's `TcpListener`): on Linux such sockets
/// conflict only when one of them listens. A listening probe held that way took the port from the
/// node, or from a test binding it. `SO_REUSEADDR` is set on Unix only: on Windows it lets a bind
/// take over a listener, and no child inherits the probe there. On 127.0.0.1, which always
/// exists, any other probe error fails the test; on `::1` it means no IPv6, so the port counts
/// as free.
pub fn free_port() -> u16 {
    static NEXT: Mutex<Option<u32>> = Mutex::new(None);
    let low = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|range| range.split_whitespace().next()?.parse::<u16>().ok())
        .unwrap_or(32768)
        .min(32768);
    let base = low.saturating_sub(8192).max(1024);
    let span = u32::from(low.saturating_sub(base));
    let probe = |ip: IpAddr, port: u16| -> std::io::Result<u16> {
        let addr = SocketAddr::new(ip, port);
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        socket.bind(&addr.into())?;
        Ok(socket.local_addr()?.as_socket().map_or(port, |bound| bound.port()))
    };
    let in_use = |port: u16| {
        use std::io::ErrorKind::AddrInUse;
        let v4 = match probe(Ipv4Addr::LOCALHOST.into(), port) {
            Err(e) if e.kind() != AddrInUse => panic!("probing 127.0.0.1 port {port}: {e}"),
            result => result.is_err(),
        };
        v4 || probe(Ipv6Addr::LOCALHOST.into(), port).is_err_and(|e| e.kind() == AddrInUse)
    };
    if span < 1024 {
        return probe(Ipv4Addr::LOCALHOST.into(), 0).unwrap();
    }
    let mut next = NEXT.lock().unwrap();
    let start = *next.get_or_insert_with(|| {
        let nanos =
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().subsec_nanos();
        (std::process::id() ^ nanos) % span
    });
    let found = (0..span).map(|i| (start + i) % span).find(|&i| !in_use(base + i as u16));
    let i = found.expect("a free port below the ephemeral range");
    *next = Some((i + 1) % span);
    base + i as u16
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// One test, so no other test's picks move the process's next port between these.
    #[test]
    fn ports_are_free_below_the_ephemeral_range_and_skip_listeners() {
        let low = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
            .map_or(32768, |range| range.split_whitespace().next().unwrap().parse().unwrap());
        if low < 2048 {
            return; // `free_port` falls back to `bind(0)`
        }
        let first = free_port();
        assert!(first < low, "port {first} is in the ephemeral range from {low}");
        // A free port binds at once.
        drop(TcpListener::bind((Ipv4Addr::LOCALHOST, first)).unwrap());

        // The next ports, held by a listener on 127.0.0.1 and one on ::1, are skipped.
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, first + 1));
        let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, first + 2));
        let next = free_port();
        assert_ne!(next, first, "a port is handed out once");
        if v4.is_ok() {
            assert_ne!(next, first + 1, "127.0.0.1 has a listener on that port");
        }
        if v6.is_ok() {
            assert_ne!(next, first + 2, "::1 has a listener on that port");
        }
        assert!(next < low);
    }
}
