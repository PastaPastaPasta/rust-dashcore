//! The socket under a connection (Core's `Sock`): non-blocking reads and sends with readiness,
//! so the per-peer tasks reproduce Core's `MSG_DONTWAIT` byte counts (P2a §3.8.7).
//!
//! [`TcpSocket`] is the real one. [`MemSocket`] is an in-memory pair for `sim` and tests, with a
//! settable write budget to produce partial writes at chosen byte counts.

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;
use tokio::io::Interest;
use tokio::net::TcpStream;

/// A connected, non-blocking socket.
pub trait NetSocket: Send + Sync {
    /// Ready to read, or closed or failed (a read then reports which).
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    /// Core's `recv(…, MSG_DONTWAIT)`: `Ok(0)` when the other side closed, `WouldBlock` when
    /// nothing is buffered.
    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize>;

    /// `recv(…, MSG_PEEK)` of one byte once the socket is readable: `Ok(1)` when bytes wait,
    /// `Ok(0)` when the other side closed and nothing is left, or the socket's error. Consumes
    /// nothing.
    fn poll_peek(&self, cx: &mut Context<'_>) -> Poll<io::Result<usize>>;

    /// Ready to send.
    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    /// Core's `send(…, MSG_NOSIGNAL | MSG_DONTWAIT [| MSG_MORE])`: the bytes the socket took,
    /// possibly fewer than `buf.len()`, or `WouldBlock`.
    fn try_send(&self, buf: &[u8], more: bool) -> io::Result<usize>;

    /// `shutdown(SHUT_WR)`, so the other side reads end-of-stream.
    fn shutdown_write(&self) -> io::Result<()>;

    /// Core's `CloseSocketDisconnect`: closes both directions now, although per-peer tasks may
    /// still hold the socket.
    fn close(&self);
}

/// A TCP connection on the host's runtime, with `TCP_NODELAY` as Core sets it.
pub struct TcpSocket {
    stream: TcpStream,
}

impl TcpSocket {
    /// Wraps a connected stream: sets `TCP_NODELAY` (Core's `SetSocketNoDelay`) and, where
    /// `MSG_NOSIGNAL` does not exist, `SO_NOSIGPIPE`.
    ///
    /// # Errors
    ///
    /// When the socket options cannot be set.
    pub fn new(stream: TcpStream) -> io::Result<Self> {
        // Core logs a failure and keeps the connection.
        // Core: netbase.cpp:556-557 CreateSockOS; net.cpp:1927-1929 CreateNodeFromAcceptedSocket
        if stream.set_nodelay(true).is_err() {
            tracing::debug!(target: "net", "Unable to set TCP_NODELAY on a newly created socket, continuing anyway");
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos"
        ))]
        socket2::SockRef::from(&stream).set_nosigpipe(true)?;
        Ok(Self {
            stream,
        })
    }

    /// The underlying stream.
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }
}

#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn send_flags(_more: bool) -> libc::c_int {
    libc::MSG_NOSIGNAL
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn send_flags(more: bool) -> libc::c_int {
    libc::MSG_NOSIGNAL
        | if more {
            libc::MSG_MORE
        } else {
            0
        }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
fn send_flags(_more: bool) -> std::ffi::c_int {
    0
}

impl NetSocket for TcpSocket {
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream.poll_read_ready(cx)
    }

    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.try_read(buf)
    }

    fn poll_peek(&self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut byte = [0u8; 1];
        self.stream.poll_peek(cx, &mut tokio::io::ReadBuf::new(&mut byte))
    }

    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream.poll_write_ready(cx)
    }

    fn try_send(&self, buf: &[u8], more: bool) -> io::Result<usize> {
        self.stream.try_io(Interest::WRITABLE, || {
            socket2::SockRef::from(&self.stream).send_with_flags(buf, send_flags(more))
        })
    }

    fn shutdown_write(&self) -> io::Result<()> {
        socket2::SockRef::from(&self.stream).shutdown(std::net::Shutdown::Write)
    }

    fn close(&self) {
        let _ = socket2::SockRef::from(&self.stream).shutdown(std::net::Shutdown::Both);
    }
}

/// A connection to a proxy's UNIX domain socket (`-proxy=unix:…`, `-onion=unix:…`), which then
/// carries the peer's bytes as a [`TcpSocket`] does. Core sets no `TCP_NODELAY` on it
/// (netbase.cpp:550-552) and sends with the same flags.
#[cfg(unix)]
pub struct UnixSocket {
    stream: tokio::net::UnixStream,
}

#[cfg(unix)]
impl UnixSocket {
    /// Wraps a connected stream, with `SO_NOSIGPIPE` where `MSG_NOSIGNAL` does not exist.
    ///
    /// # Errors
    ///
    /// When the socket option cannot be set.
    pub fn new(stream: tokio::net::UnixStream) -> io::Result<Self> {
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos"
        ))]
        socket2::SockRef::from(&stream).set_nosigpipe(true)?;
        Ok(Self {
            stream,
        })
    }
}

#[cfg(unix)]
impl NetSocket for UnixSocket {
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream.poll_read_ready(cx)
    }

    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.try_read(buf)
    }

    fn poll_peek(&self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        loop {
            std::task::ready!(self.stream.poll_read_ready(cx))?;
            let mut byte = [std::mem::MaybeUninit::<u8>::uninit()];
            match self.stream.try_io(Interest::READABLE, || {
                socket2::SockRef::from(&self.stream).recv_with_flags(&mut byte, libc::MSG_PEEK)
            }) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                r => return Poll::Ready(r),
            }
        }
    }

    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream.poll_write_ready(cx)
    }

    fn try_send(&self, buf: &[u8], more: bool) -> io::Result<usize> {
        self.stream.try_io(Interest::WRITABLE, || {
            socket2::SockRef::from(&self.stream).send_with_flags(buf, send_flags(more))
        })
    }

    fn shutdown_write(&self) -> io::Result<()> {
        socket2::SockRef::from(&self.stream).shutdown(std::net::Shutdown::Write)
    }

    fn close(&self) {
        let _ = socket2::SockRef::from(&self.stream).shutdown(std::net::Shutdown::Both);
    }
}

/// Core's `NetworkErrorString` off Windows, which is `SysErrorString`: the system's message,
/// then the code in parentheses.
// Core: util/sock.cpp NetworkErrorString, util/syserror.cpp SysErrorString
pub fn network_error_string(e: &io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(code) => {
            let msg = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text);
            format!("{msg} ({code})")
        }
        None => text,
    }
}

/// One direction of a [`MemSocket`] pair.
#[derive(Default)]
struct Pipe {
    buf: VecDeque<u8>,
    /// Bytes the writer may still send before it would block; `None` is unlimited.
    budget: Option<usize>,
    closed: bool,
    reader: Option<Waker>,
    writer: Option<Waker>,
    /// `more` of every accepted send, for tests of `MSG_MORE` coalescing.
    sends: Vec<(usize, bool)>,
}

/// One end of an in-memory connection.
#[derive(Clone)]
pub struct MemSocket {
    rx: Arc<Mutex<Pipe>>,
    tx: Arc<Mutex<Pipe>>,
}

impl MemSocket {
    /// A connected pair; what one end sends, the other reads.
    pub fn pair() -> (Self, Self) {
        let a = Arc::new(Mutex::new(Pipe::default()));
        let b = Arc::new(Mutex::new(Pipe::default()));
        (
            Self {
                rx: a.clone(),
                tx: b.clone(),
            },
            Self {
                rx: b,
                tx: a,
            },
        )
    }

    /// Limits what this end may still send before a send would block; `None` lifts the limit.
    /// A send that crosses the limit is partial, as a full kernel buffer makes it.
    pub fn set_send_budget(&self, budget: Option<usize>) {
        let mut tx = self.tx.lock();
        tx.budget = budget;
        if let Some(w) = tx.writer.take() {
            w.wake();
        }
    }

    /// Everything this end has sent and the other end not yet read.
    pub fn in_flight(&self) -> usize {
        self.tx.lock().buf.len()
    }

    /// The size and `more` flag of every send this end accepted.
    pub fn sends(&self) -> Vec<(usize, bool)> {
        self.tx.lock().sends.clone()
    }

    /// Takes everything the other end has sent to this one, without readiness.
    pub fn drain(&self) -> Vec<u8> {
        let mut rx = self.rx.lock();
        let out: Vec<u8> = rx.buf.drain(..).collect();
        if let Some(w) = rx.writer.take() {
            w.wake();
        }
        out
    }

    /// Writes `bytes` to the other end, ignoring the budget.
    pub fn inject(&self, bytes: &[u8]) {
        let mut tx = self.tx.lock();
        tx.buf.extend(bytes);
        if let Some(w) = tx.reader.take() {
            w.wake();
        }
    }

    /// Closes both directions, as the peer closing its socket.
    pub fn close(&self) {
        for pipe in [&self.rx, &self.tx] {
            let mut p = pipe.lock();
            p.closed = true;
            if let Some(w) = p.reader.take() {
                w.wake();
            }
            if let Some(w) = p.writer.take() {
                w.wake();
            }
        }
    }
}

impl NetSocket for MemSocket {
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut rx = self.rx.lock();
        if !rx.buf.is_empty() || rx.closed {
            Poll::Ready(Ok(()))
        } else {
            rx.reader = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut rx = self.rx.lock();
        if rx.buf.is_empty() {
            return if rx.closed {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let n = buf.len().min(rx.buf.len());
        for (dst, src) in buf.iter_mut().zip(rx.buf.drain(..n)) {
            *dst = src;
        }
        if let Some(w) = rx.writer.take() {
            w.wake();
        }
        Ok(n)
    }

    fn poll_peek(&self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut rx = self.rx.lock();
        if !rx.buf.is_empty() {
            Poll::Ready(Ok(1))
        } else if rx.closed {
            Poll::Ready(Ok(0))
        } else {
            rx.reader = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut tx = self.tx.lock();
        if tx.closed || tx.budget != Some(0) {
            Poll::Ready(Ok(()))
        } else {
            tx.writer = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn try_send(&self, buf: &[u8], more: bool) -> io::Result<usize> {
        let mut tx = self.tx.lock();
        if tx.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = tx.budget.map_or(buf.len(), |b| b.min(buf.len()));
        if n == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(b) = tx.budget.as_mut() {
            *b -= n;
        }
        tx.buf.extend(&buf[..n]);
        tx.sends.push((n, more));
        if let Some(w) = tx.reader.take() {
            w.wake();
        }
        Ok(n)
    }

    fn shutdown_write(&self) -> io::Result<()> {
        let mut tx = self.tx.lock();
        tx.closed = true;
        if let Some(w) = tx.reader.take() {
            w.wake();
        }
        Ok(())
    }

    fn close(&self) {
        MemSocket::close(self);
    }
}
