//! A scripted peer's socket and P2P transport, with the framing both scripted peers share.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use dash_p2p_transport::{CommandString, Magic, NetMessage, OutMsg, Transport};

pub struct Wire {
    pub sock: TcpStream,
    pub t: Transport,
    pub inbox: VecDeque<NetMessage>,
}

impl Wire {
    /// `sock` speaking V1.
    pub fn v1(sock: TcpStream, magic: Magic) -> Wire {
        Wire {
            sock,
            t: Transport::v1(magic),
            inbox: VecDeque::new(),
        }
    }

    /// `sock` as a V2 responder, which also takes V1.
    pub fn v2(sock: TcpStream, magic: Magic) -> Wire {
        Wire {
            sock,
            t: Transport::v2(magic, false).expect("rng"),
            inbox: VecDeque::new(),
        }
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        loop {
            let data = self.t.bytes_to_send(false).data;
            if data.is_empty() {
                return Ok(());
            }
            let n = self.sock.write(data)?;
            self.t.mark_bytes_sent(n);
        }
    }

    /// Sends what is pending and feeds what arrives; `Err` on end-of-stream or bytes the
    /// transport refuses.
    pub fn pump(&mut self) -> std::io::Result<()> {
        self.flush()?;
        let mut buf = [0u8; 65536];
        match self.sock.read(&mut buf) {
            Ok(0) => Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                let inbox = &mut self.inbox;
                self.t
                    .receive(&buf[..n], 0, |m| inbox.extend(m.ok()))
                    .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Queues the message, pumping until the transport takes it (failing after `patience`), then
    /// flushes.
    pub fn send(
        &mut self,
        command: &str,
        payload: Vec<u8>,
        patience: Duration,
    ) -> std::io::Result<()> {
        let mut msg = OutMsg::new(CommandString::from_static(command), payload);
        let deadline = Instant::now() + patience;
        while let Err(m) = self.t.set_message_to_send(msg) {
            msg = m;
            self.pump()?;
            assert!(Instant::now() < deadline, "the transport never took {command}");
        }
        self.flush()
    }
}
