//! Keeps the JSON-RPC stream on a private fd: whatever else writes to fd 1
//! (a plugin's printf, a stray println!) goes to stderr instead.

#[cfg(unix)]
pub use unix::{ProtocolOut, protect_stdout};

#[cfg(not(unix))]
pub use other::{ProtocolOut, protect_stdout};

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io;
    use std::os::fd::{FromRawFd, RawFd};

    pub type ProtocolOut = File;

    /// Moves the protocol onto a duplicate of `stdout` and points `stdout`
    /// at `stderr`. Must run before any plugin can load.
    pub(crate) fn divert_fd(stdout: RawFd, stderr: RawFd) -> io::Result<File> {
        // SAFETY: plain fd syscalls; the new fd is owned by the returned File.
        unsafe {
            let saved = libc::fcntl(stdout, libc::F_DUPFD_CLOEXEC, 3);
            if saved < 0 {
                return Err(io::Error::last_os_error());
            }
            let saved = File::from_raw_fd(saved);
            if libc::dup2(stderr, stdout) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(saved)
        }
    }

    pub fn protect_stdout() -> io::Result<ProtocolOut> {
        divert_fd(libc::STDOUT_FILENO, libc::STDERR_FILENO)
    }
}

#[cfg(not(unix))]
mod other {
    pub struct ProtocolOut;

    pub fn protect_stdout() -> std::io::Result<ProtocolOut> {
        Ok(ProtocolOut)
    }
}

#[cfg(all(test, unix))]
use unix::divert_fd;

#[cfg(all(test, unix))]
#[path = "tests/stdout_guard.rs"]
mod tests;
