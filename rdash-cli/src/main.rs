//! The `rdash-cli` binary.

#[cfg(unix)]
mod sigpipe;

use std::ffi::OsString;
use std::io::Write as _;

fn main() {
    // dash-cli keeps the SIGPIPE disposition it inherited; Rust starts a program with SIGPIPE
    // ignored. Where no constructor read the inherited one, the default, as a shell gives.
    #[cfg(unix)]
    if !sigpipe::restore_inherited() {
        // SAFETY: restoring the default action of a signal, before any thread exists.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
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
                let message = rdash_cli::non_utf8_argument_message(position, &arg, after_command);
                let _ = writeln!(std::io::stderr(), "{message}");
                std::process::exit(1);
            }
        }
    }
    std::process::exit(rdash_cli::main_with(&argv));
}

#[cfg(test)]
mod tests {
    /// DEC-1309: rdash-cli's SIGPIPE module is rdashd's, byte for byte, so neither copy can
    /// change alone.
    #[test]
    fn the_sigpipe_module_is_rdashd_s() {
        assert_eq!(include_str!("sigpipe.rs"), include_str!("../../rdashd/src/sigpipe.rs"));
    }
}
