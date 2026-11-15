//! `-daemon` and `-daemonwait`: Core's `fork_daemon` and its token pipe (`util/tokenpipe.cpp`).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Core's `TokenPipeEnd::TS_EOS`: the other end closed before writing.
pub const TS_EOS: i32 = -2;
/// Core's `TokenPipeEnd::TS_ERR`.
pub const TS_ERR: i32 = -1;

/// One end of the pipe between the daemon's parent and child. Dropping it closes it.
pub struct TokenPipeEnd(OwnedFd);

impl TokenPipeEnd {
    /// Writes one token: whether it was written.
    // Core: util/tokenpipe.cpp:30-46
    pub fn token_write(&self, token: u8) -> bool {
        loop {
            // SAFETY: the descriptor is open; one readable byte is passed.
            let n = unsafe { libc::write(self.0.as_raw_fd(), (&raw const token).cast(), 1) };
            match n {
                1 => return true,
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                _ => return false,
            }
        }
    }

    /// Reads one token (0–255), or [`TS_ERR`] or [`TS_EOS`].
    // Core: util/tokenpipe.cpp:48-66
    pub fn token_read(&self) -> i32 {
        let mut token = 0u8;
        loop {
            // SAFETY: the descriptor is open; `token` has room for the byte.
            let n = unsafe { libc::read(self.0.as_raw_fd(), (&raw mut token).cast(), 1) };
            match n {
                1 => return i32::from(token),
                0 => return TS_EOS,
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                _ => return TS_ERR,
            }
        }
    }
}

/// Which side of the fork this is.
pub enum Forked {
    /// The child: the pipe's write end.
    Child(TokenPipeEnd),
    /// The parent: the pipe's read end, past the child's setup token.
    Parent(TokenPipeEnd),
}

/// Core's `TokenPipe::Make`: a close-on-exec pipe (read end, write end).
// Core: util/tokenpipe.cpp:68-90
fn token_pipe() -> io::Result<(TokenPipeEnd, TokenPipeEnd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for both ends.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `pipe` opened both descriptors; each is owned once.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&read, &write] {
        // SAFETY: the descriptor is open.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((TokenPipeEnd(read), TokenPipeEnd(write)))
}

/// Core's `fork_daemon`, glibc `daemon()`'s order of operations with a pipe to the parent. Must
/// run while the process has one thread.
// Core: bitcoind.cpp:50-106
pub fn fork_daemon(nochdir: bool, noclose: bool) -> io::Result<Forked> {
    let (read, write) = token_pipe()?;
    // SAFETY: the process is single-threaded here (the caller's contract), so the child may run
    // anything.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid != 0 {
        drop(write);
        if read.token_read() != 0 {
            // Something went wrong while setting up the child.
            return Err(io::Error::last_os_error());
        }
        return Ok(Forked::Parent(read));
    }
    drop(read);
    // SAFETY: plain system calls in the single-threaded child.
    unsafe {
        if libc::setsid() < 0 {
            std::process::exit(1);
        }
        if !nochdir && libc::chdir(c"/".as_ptr()) != 0 {
            std::process::exit(1);
        }
        if !noclose {
            // Detach from the terminal: /dev/null as stdin, stdout and stderr.
            let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if fd < 0 {
                std::process::exit(1);
            }
            let err = libc::dup2(fd, libc::STDIN_FILENO) < 0
                || libc::dup2(fd, libc::STDOUT_FILENO) < 0
                || libc::dup2(fd, libc::STDERR_FILENO) < 0;
            // Kept if fd <= 2: the program may have started without them.
            if fd > 2 {
                libc::close(fd);
            }
            if err {
                std::process::exit(1);
            }
        }
    }
    write.token_write(0);
    Ok(Forked::Child(write))
}
