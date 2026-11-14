//! `Socks5` against scripted proxies: the bytes Core sends (feature_proxy.py's
//! `Socks5Connection` reads the same), each reply Core accepts, and each failure and its log.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

use super::*;

const T: Duration = SOCKS5_RECV_TIMEOUT;

fn never() -> bool {
    false
}

fn creds(n: &str) -> ProxyCredentials {
    ProxyCredentials {
        username: n.into(),
        password: n.into(),
    }
}

/// Reads exactly `n` bytes the client sent.
async fn read_n(proxy: &mut DuplexStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    proxy.read_exact(&mut buf).await.unwrap();
    buf
}

/// The client's `CONNECT` request for `dest:port`.
fn connect_request(dest: &str, port: u16) -> Vec<u8> {
    let mut req = vec![5, 1, 0, 3, dest.len() as u8];
    req.extend_from_slice(dest.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    req
}

/// Runs `socks5` against a proxy task `serve`, returning the client's result.
async fn run<F, Fut>(dest: &str, port: u16, auth: Option<ProxyCredentials>, serve: F) -> bool
where
    F: FnOnce(DuplexStream) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (mut client, proxy) = duplex(1024);
    let server = tokio::spawn(serve(proxy));
    let ok = socks5(dest, port, auth.as_ref(), &mut client, T, &never).await;
    drop(client);
    server.await.unwrap();
    ok
}

#[tokio::test]
async fn no_auth_connect_sends_core_bytes() {
    let ok = run("15.61.23.23", 1234, None, |mut p| async move {
        assert_eq!(read_n(&mut p, 3).await, [5, 1, 0]);
        p.write_all(&[5, 0]).await.unwrap();
        assert_eq!(read_n(&mut p, 18).await, connect_request("15.61.23.23", 1234));
        // Bound address as IPv4, then the port.
        p.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0x1f, 0x90]).await.unwrap();
    })
    .await;
    assert!(ok);
}

#[tokio::test]
async fn randomized_credentials_use_rfc1929() {
    let ok = run(
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion",
        8333,
        Some(creds("7")),
        |mut p| async move {
            assert_eq!(read_n(&mut p, 4).await, [5, 2, 0, 2]);
            p.write_all(&[5, 2]).await.unwrap();
            assert_eq!(read_n(&mut p, 5).await, [1, 1, b'7', 1, b'7']);
            p.write_all(&[1, 0]).await.unwrap();
            let dest = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
            assert_eq!(read_n(&mut p, 7 + dest.len()).await, connect_request(dest, 8333));
            // Bound address as a domain name.
            p.write_all(&[5, 0, 0, 3, 3, b'a', b'b', b'c', 0, 1]).await.unwrap();
        },
    )
    .await;
    assert!(ok);
}

#[tokio::test]
async fn offered_credentials_may_be_declined() {
    // Tor answers NOAUTH to a client offering both (feature_proxy's conf1 is unauth only).
    let ok = run("node.noumenon", 8333, Some(creds("0")), |mut p| async move {
        assert_eq!(read_n(&mut p, 4).await, [5, 2, 0, 2]);
        p.write_all(&[5, 0]).await.unwrap();
        assert_eq!(read_n(&mut p, 20).await, connect_request("node.noumenon", 8333));
        let mut reply = vec![5, 0, 0, 4];
        reply.extend_from_slice(&[0; 18]);
        p.write_all(&reply).await.unwrap();
    })
    .await;
    assert!(ok);
}

#[tokio::test]
async fn ipv6_destination_goes_as_text() {
    let dest = "1233:3432:2434:2343:3234:2345:6546:4534";
    let ok = run(dest, 5443, None, move |mut p| async move {
        read_n(&mut p, 3).await;
        p.write_all(&[5, 0]).await.unwrap();
        assert_eq!(read_n(&mut p, 7 + dest.len()).await, connect_request(dest, 5443));
        p.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
    })
    .await;
    assert!(ok);
}

/// Each bound address form in the reply (IPv4, a domain name, IPv6) is read whole with its
/// port, and the stream is left at the peer's first byte.
// Core: netbase.cpp:476-503
#[tokio::test]
async fn each_bound_address_is_read_whole() {
    let ipv6: Vec<u8> = [4].into_iter().chain(1..=16u8).collect();
    let bounds: [&[u8]; 3] = [&[1, 10, 20, 30, 40], &[3, 3, b'a', b'b', b'c'], &ipv6];
    for bound in bounds {
        let (mut client, mut p) = duplex(1024);
        let reply = [&[5, 0, 0][..], bound, &0x208du16.to_be_bytes(), b"PEER"].concat();
        let server = tokio::spawn(async move {
            read_n(&mut p, 3).await;
            p.write_all(&[5, 0]).await.unwrap();
            read_n(&mut p, 7 + "node.noumenon".len()).await;
            p.write_all(&reply).await.unwrap();
            p
        });
        assert!(socks5("node.noumenon", 8333, None, &mut client, T, &never).await, "{bound:?}");
        let _proxy = server.await.unwrap();
        let mut peer = [0; 4];
        client.read_exact(&mut peer).await.unwrap();
        assert_eq!(&peer, b"PEER", "{bound:?}");
    }
}

/// A proxy that answers the method selection with `method` and closes.
async fn method_reply(method: [u8; 2], auth: Option<ProxyCredentials>) -> bool {
    run("a", 1, auth, move |mut p| async move {
        let _ = p.read(&mut [0; 4]).await;
        p.write_all(&method).await.unwrap();
    })
    .await
}

#[tokio::test]
async fn method_selection_failures() {
    // Not SOCKS5, from a proxy that would grant the request: the client stops at the reply.
    let ok = run("a", 1, None, |mut p| async move {
        read_n(&mut p, 3).await;
        p.write_all(&[4, 0]).await.unwrap();
        let mut rest = Vec::<u8>::new();
        p.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "nothing after the reply: {rest:?}");
    })
    .await;
    assert!(!ok);
    // USER_PASS without credentials offered, GSSAPI, or no acceptable method.
    assert!(!method_reply([5, 2], None).await);
    assert!(!method_reply([5, 1], None).await);
    assert!(!method_reply([5, 0xff], Some(creds("1"))).await);
}

#[tokio::test]
async fn authentication_rejected() {
    let ok = run("a", 1, Some(creds("1")), |mut p| async move {
        read_n(&mut p, 4).await;
        p.write_all(&[5, 2]).await.unwrap();
        read_n(&mut p, 5).await;
        p.write_all(&[1, 1]).await.unwrap();
    })
    .await;
    assert!(!ok);
    // A sub-negotiation version other than 1 fails too.
    let ok = run("a", 1, Some(creds("1")), |mut p| async move {
        read_n(&mut p, 4).await;
        p.write_all(&[5, 2]).await.unwrap();
        read_n(&mut p, 5).await;
        p.write_all(&[5, 0]).await.unwrap();
    })
    .await;
    assert!(!ok);
}

/// A proxy that accepts NOAUTH and answers the request with `reply`, then closes.
async fn request_reply(reply: Vec<u8>) -> bool {
    run("a", 1, None, move |mut p| async move {
        read_n(&mut p, 3).await;
        p.write_all(&[5, 0]).await.unwrap();
        read_n(&mut p, 8).await;
        p.write_all(&reply).await.unwrap();
    })
    .await
}

#[tokio::test]
async fn request_failures() {
    // Each REP code is a failure (Core logs Socks5ErrorString's text).
    for rep in 1..=9u8 {
        assert!(!request_reply(vec![5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await, "rep {rep}");
    }
    // Wrong version, reserved byte, unknown ATYP.
    assert!(!request_reply(vec![4, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await);
    assert!(!request_reply(vec![5, 0, 1, 1, 0, 0, 0, 0, 0, 0]).await);
    assert!(!request_reply(vec![5, 0, 0, 2, 0, 0, 0, 0, 0, 0]).await);
    // Truncated bound address, truncated port, missing domain length.
    assert!(!request_reply(vec![5, 0, 0, 1, 0, 0]).await);
    assert!(!request_reply(vec![5, 0, 0, 1, 0, 0, 0, 0, 0]).await);
    assert!(!request_reply(vec![5, 0, 0, 3]).await);
    // Disconnect before the reply header.
    assert!(!request_reply(vec![5, 0]).await);
    // An empty domain name is read as zero bytes (Core's `InterruptibleRecv` of 0 is OK).
    assert!(request_reply(vec![5, 0, 0, 3, 0, 0, 0]).await);
}

#[tokio::test]
async fn overlong_names_are_refused_before_sending() {
    let (mut client, mut proxy) = duplex(1024);
    assert!(!socks5(&"x".repeat(256), 1, None, &mut client, T, &never).await);
    drop(client);
    let mut sent = Vec::new();
    proxy.read_to_end(&mut sent).await.unwrap();
    assert!(sent.is_empty());
    // Over-long credentials fail after the method selection, before the sub-negotiation.
    let ok = run("a", 1, Some(creds(&"x".repeat(256))), |mut p| async move {
        assert_eq!(read_n(&mut p, 4).await, [5, 2, 0, 2]);
        p.write_all(&[5, 2]).await.unwrap();
        let mut rest = Vec::<u8>::new();
        p.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    })
    .await;
    assert!(!ok);
}

#[tokio::test]
async fn a_255_byte_name_is_sent() {
    let dest = "y".repeat(255);
    let d = dest.clone();
    let ok = run(&dest, 80, None, move |mut p| async move {
        read_n(&mut p, 3).await;
        p.write_all(&[5, 0]).await.unwrap();
        assert_eq!(read_n(&mut p, 7 + 255).await, connect_request(&d, 80));
        p.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
    })
    .await;
    assert!(ok);
}

#[tokio::test(start_paused = true)]
async fn each_read_has_its_own_window() {
    // A proxy that answers every read after 19 s passes: each read's window is 20 s.
    let ok = run("a", 1, None, |mut p| async move {
        read_n(&mut p, 3).await;
        tokio::time::sleep(Duration::from_secs(19)).await;
        p.write_all(&[5, 0]).await.unwrap();
        read_n(&mut p, 8).await;
        tokio::time::sleep(Duration::from_secs(19)).await;
        p.write_all(&[5, 0, 0, 1]).await.unwrap();
        tokio::time::sleep(Duration::from_secs(19)).await;
        p.write_all(&[0, 0, 0, 0]).await.unwrap();
        tokio::time::sleep(Duration::from_secs(19)).await;
        p.write_all(&[0, 0]).await.unwrap();
    })
    .await;
    assert!(ok);
}

#[tokio::test(start_paused = true)]
async fn a_silent_proxy_times_out() {
    for silent_after_method in [false, true] {
        let (mut client, mut p) = duplex(1024);
        let server = tokio::spawn(async move {
            read_n(&mut p, 3).await;
            if silent_after_method {
                p.write_all(&[5, 0]).await.unwrap();
            }
            // Holds the connection open past the window.
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let start = Instant::now();
        assert!(!socks5("a", 1, None, &mut client, T, &never).await);
        assert_eq!(start.elapsed(), T);
        server.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_ends_a_read_within_a_second() {
    let stop = AtomicBool::new(false);
    let interrupted = || stop.load(Ordering::SeqCst);
    let (mut client, mut p) = duplex(1024);
    let server = tokio::spawn(async move {
        read_n(&mut p, 3).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let start = Instant::now();
    let read = socks5("a", 1, None, &mut client, T, &interrupted);
    let flip = async {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        stop.store(true, Ordering::SeqCst);
    };
    let (ok, ()) = tokio::join!(read, flip);
    assert!(!ok);
    // Checked once per MAX_WAIT_FOR_IO.
    assert_eq!(start.elapsed(), Duration::from_secs(3));
    server.abort();
}

#[test]
fn unix_socket_paths() {
    #[cfg(unix)]
    {
        assert!(is_unix_socket_path("unix:/a/b"));
        assert!(is_unix_socket_path(&format!("unix:{}", "x".repeat(SUN_PATH_LEN - 1))));
        assert!(!is_unix_socket_path(&format!("unix:{}", "x".repeat(SUN_PATH_LEN))));
    }
    assert!(!is_unix_socket_path("/a/b"));
    assert!(!is_unix_socket_path("127.0.0.1:9050"));
}

#[test]
fn proxy_table() {
    let mut t = ProxyTable::default();
    let p = |port| Proxy {
        addr: ProxyAddr::Service(Service::new(NetAddr::ipv4([127, 0, 0, 1]), port)),
        randomize_credentials: true,
    };
    t.set(Network::Ipv4, p(9050));
    t.set(
        Network::Onion,
        Proxy {
            addr: ProxyAddr::Unix("unix:/run/tor".into()),
            randomize_credentials: false,
        },
    );
    assert_eq!(t.get(Network::Ipv4).unwrap().to_string(), "127.0.0.1:9050");
    assert_eq!(t.get(Network::Onion).unwrap().to_string(), "unix:/run/tor");
    assert!(t.get(Network::Ipv6).is_none() && t.name_proxy().is_none());
    // `IsProxy` ignores the port; an unset entry matches `::`.
    assert!(t.is_proxy(&NetAddr::ipv4([127, 0, 0, 1])));
    assert!(!t.is_proxy(&NetAddr::ipv4([127, 0, 0, 2])));
    assert!(t.is_proxy(&NetAddr::default()));
}

/// `SetProxy` and `SetNameProxy` refuse an invalid proxy (an invalid address, or a `unix:` path
/// that does not fit `sun_path`) and keep the entry they had.
// Core: netbase.h:73-77, netbase.cpp:229-245, :684-708
#[test]
fn invalid_proxies_are_refused() {
    let service = |addr: NetAddr| Proxy {
        addr: ProxyAddr::Service(Service::new(addr, 9050)),
        randomize_credentials: false,
    };
    let unix = |path: String| Proxy {
        addr: ProxyAddr::Unix(path),
        randomize_credentials: false,
    };
    let good = service(NetAddr::ipv4([127, 0, 0, 1]));
    let mut t = ProxyTable::default();
    assert!(t.set(Network::Ipv4, good.clone()) && t.set_name_proxy(good.clone()));
    let mut invalid = vec![
        Proxy {
            addr: ProxyAddr::Service(Service::default()),
            randomize_credentials: true,
        },
        service(NetAddr::ipv4([0, 0, 0, 0])),
        unix("/run/tor".into()),
    ];
    #[cfg(unix)]
    {
        invalid.push(unix(format!("unix:{}", "x".repeat(SUN_PATH_LEN))));
        let longest = unix(format!("unix:{}", "x".repeat(SUN_PATH_LEN - 1)));
        assert!(longest.is_valid());
        let mut u = ProxyTable::default();
        assert!(u.set(Network::Onion, longest.clone()) && u.set_name_proxy(longest.clone()));
        assert_eq!((u.get(Network::Onion), u.name_proxy()), (Some(&longest), Some(&longest)));
    }
    for proxy in invalid {
        assert!(!proxy.is_valid(), "{proxy:?}");
        assert!(!t.set(Network::Ipv4, proxy.clone()), "{proxy:?}");
        assert!(!t.set(Network::Ipv6, proxy.clone()), "{proxy:?}");
        assert!(!t.set_name_proxy(proxy.clone()), "{proxy:?}");
        assert_eq!(t.get(Network::Ipv4), Some(&good));
        assert_eq!(t.get(Network::Ipv6), None);
        assert_eq!(t.name_proxy(), Some(&good));
    }
}
