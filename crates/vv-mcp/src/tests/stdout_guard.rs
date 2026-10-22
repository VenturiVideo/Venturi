use super::divert_fd;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

fn pipe() -> (File, File) {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    assert_eq!(
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
}

fn read_available(mut reader: File) -> Vec<u8> {
    let mut buf = [0; 64];
    let n = reader.read(&mut buf).unwrap_or(0);
    buf[..n].to_vec()
}

#[test]
fn stray_stdout_writes_go_to_stderr_and_the_saved_fd_reaches_the_protocol() {
    let (protocol_rx, stdout_tx) = pipe();
    let (stderr_rx, stderr_tx) = pipe();
    let stdout_fd: OwnedFd = stdout_tx.into();
    let stderr_fd: OwnedFd = stderr_tx.into();

    let mut saved = divert_fd(stdout_fd.as_raw_fd(), stderr_fd.as_raw_fd()).unwrap();

    let mut stray = File::from(stdout_fd);
    stray.write_all(b"plugin noise").unwrap();
    saved.write_all(b"{\"jsonrpc\"}").unwrap();

    assert_eq!(read_available(stderr_rx), b"plugin noise");
    assert_eq!(read_available(protocol_rx), b"{\"jsonrpc\"}");
}

#[test]
fn the_saved_fd_is_close_on_exec() {
    let (_rx, stdout_tx) = pipe();
    let (_rx2, stderr_tx) = pipe();
    let saved = divert_fd(stdout_tx.as_raw_fd(), stderr_tx.as_raw_fd()).unwrap();
    let flags = unsafe { libc::fcntl(saved.as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}
