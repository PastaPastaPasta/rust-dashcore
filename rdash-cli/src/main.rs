//! The `rdash-cli` binary.

use std::ffi::OsString;

fn main() {
    // Rust starts a program with SIGPIPE ignored; Core's dash-cli dies of it (exit status 141
    // under a shell) when its output is a closed pipe.
    #[cfg(unix)]
    // SAFETY: restoring the default action of a signal, before any thread exists.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut argv = Vec::new();
    // Options end at the first argument that is not one; what follows is the command's.
    let mut after_command = false;
    for (position, arg) in std::env::args_os().enumerate() {
        match OsString::into_string(arg) {
            Ok(arg) => {
                after_command |= position > 0
                    && !(arg.starts_with('-') || cfg!(windows) && arg.starts_with('/'));
                argv.push(arg);
            }
            Err(arg) => {
                // Arguments are UTF-8 here (a deviation from Core, which passes the bytes on).
                // An argument may be a password: nothing of it is echoed but its position and,
                // for an option, the plain `-name` before its `=`.
                eprintln!(
                    "{}",
                    rdash_cli::non_utf8_argument_message(position, &arg, after_command)
                );
                std::process::exit(1);
            }
        }
    }
    std::process::exit(rdash_cli::main_with(&argv));
}
