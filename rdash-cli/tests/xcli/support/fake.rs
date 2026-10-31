//! A scripted HTTP server for the XCLI cases dashd cannot produce: connection drops, malformed
//! replies, HTTP error statuses, a node in warm-up, and capturing the exact bytes a client sends.

use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

type Writer = dyn Fn(&mut dyn Write) -> std::io::Result<()> + Send + Sync;

/// Writes a reply too large to hold in memory.
#[derive(Clone)]
pub struct Generator(Arc<Writer>);

impl Generator {
    pub fn new(f: impl Fn(&mut dyn Write) -> std::io::Result<()> + Send + Sync + 'static) -> Self {
        Generator(Arc::new(f))
    }
}

impl fmt::Debug for Generator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Generator")
    }
}

/// What the server does with a connection.
#[derive(Clone, Debug)]
pub enum Reply {
    /// Write these bytes, then close.
    Raw(Vec<u8>),
    /// Close without answering.
    Close,
    /// Keep the connection open without answering for this long.
    Hang(Duration),
    /// Wait, then write each piece and wait after it, so the client sees them as separate reads.
    Pieces(Duration, Vec<(Vec<u8>, Duration)>),
    /// Write whatever the generator writes, until it is done or the client has gone away.
    Stream(Generator),
}

impl Reply {
    /// `HTTP/1.1 <status line>` with a `Content-Length` body.
    pub fn http(status: &str, body: &str) -> Reply {
        Reply::Raw(
            format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                .into_bytes(),
        )
    }

    /// A 200 reply with a JSON-RPC body.
    pub fn json(body: &str) -> Reply {
        Reply::http("200 OK", body)
    }
}

type Handler = dyn Fn(usize, &[u8]) -> Reply + Send + Sync;

/// A server on a free local port, recording every request it receives.
pub struct FakeServer {
    pub port: u16,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    /// `handler(connection_index, request_bytes)` decides the reply to each connection, counting
    /// from 0.
    pub fn start(handler: impl Fn(usize, &[u8]) -> Reply + Send + Sync + 'static) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let thread = {
            let (requests, stop) = (requests.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut index = 0;
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let handler = handler.clone();
                            let requests = requests.clone();
                            let i = index;
                            index += 1;
                            // One thread per connection, so a hanging reply does not block the next
                            // client; the request is recorded before the reply is chosen.
                            std::thread::spawn(move || serve(stream, i, &*handler, &requests));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        FakeServer {
            port,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// A server that answers every request with `reply`.
    pub fn always(reply: Reply) -> FakeServer {
        FakeServer::start(move |_, _| reply.clone())
    }

    /// The requests received so far, in order.
    pub fn requests(&self) -> Vec<Vec<u8>> {
        self.requests.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.requests.lock().unwrap().clear();
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(mut stream: TcpStream, index: usize, handler: &Handler, requests: &Mutex<Vec<Vec<u8>>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut want: Option<usize> = None;
    loop {
        if want.is_none()
            && let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            want = Some(end + 4 + length);
        }
        if want.is_some_and(|w| data.len() >= w) {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
        }
    }
    requests.lock().unwrap().push(data.clone());
    match handler(index, &data) {
        Reply::Raw(bytes) => {
            let _ = stream.write_all(&bytes);
        }
        Reply::Close => {}
        Reply::Hang(d) => std::thread::sleep(d),
        Reply::Stream(generate) => {
            let _ = (generate.0)(&mut stream);
        }
        Reply::Pieces(first, pieces) => {
            std::thread::sleep(first);
            for (bytes, pause) in pieces {
                let _ = stream.write_all(&bytes);
                let _ = stream.flush();
                std::thread::sleep(pause);
            }
        }
    }
}
