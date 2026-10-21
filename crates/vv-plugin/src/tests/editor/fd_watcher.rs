use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

fn counter() -> (Waker, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    (
        Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }),
        count,
    )
}

fn make_pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn write_byte(fd: &OwnedFd) {
    assert_eq!(
        unsafe { libc::write(fd.as_raw_fd(), [1u8].as_ptr().cast(), 1) },
        1
    );
}

fn read_byte(fd: &OwnedFd) {
    let mut b = [0u8; 1];
    assert_eq!(
        unsafe { libc::read(fd.as_raw_fd(), b.as_mut_ptr().cast(), 1) },
        1
    );
}

fn wait_for(count: &AtomicUsize, n: usize) {
    let end = Instant::now() + Duration::from_secs(5);
    while count.load(Ordering::SeqCst) < n {
        assert!(Instant::now() < end, "waker not called {n} times");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_readable_fd_wakes_once_until_rearmed() {
    let (waker, count) = counter();
    let watcher = FdWatcher::new(waker).unwrap();
    let (rx, tx) = make_pipe();
    watcher.set_fds(vec![rx.as_raw_fd()]);
    watcher.barrier();
    assert_eq!(count.load(Ordering::SeqCst), 0);

    write_byte(&tx);
    wait_for(&count, 1);
    watcher.barrier();
    assert_eq!(count.load(Ordering::SeqCst), 1, "no spin while unacked");

    read_byte(&rx);
    watcher.rearm();
    watcher.barrier();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    write_byte(&tx);
    wait_for(&count, 2);
}

#[test]
fn rearming_with_data_still_pending_wakes_again() {
    let (waker, count) = counter();
    let watcher = FdWatcher::new(waker).unwrap();
    let (rx, tx) = make_pipe();
    watcher.set_fds(vec![rx.as_raw_fd()]);
    write_byte(&tx);
    wait_for(&count, 1);
    watcher.rearm();
    wait_for(&count, 2);
}

#[test]
fn set_fds_replaces_the_watched_set() {
    let (waker, count) = counter();
    let watcher = FdWatcher::new(waker).unwrap();
    let (rx_a, tx_a) = make_pipe();
    let (rx_b, tx_b) = make_pipe();
    watcher.set_fds(vec![rx_a.as_raw_fd()]);
    watcher.set_fds(vec![rx_b.as_raw_fd()]);
    watcher.barrier();
    write_byte(&tx_a);
    watcher.barrier();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    write_byte(&tx_b);
    wait_for(&count, 1);
}

#[test]
fn a_set_replaced_while_polling_never_wakes_for_the_old_fds() {
    for _ in 0..200 {
        let (waker, count) = counter();
        let watcher = FdWatcher::new(waker).unwrap();
        let (rx_a, tx_a) = make_pipe();
        let (rx_b, _tx_b) = make_pipe();
        watcher.set_fds(vec![rx_a.as_raw_fd()]);
        watcher.barrier();
        watcher.set_fds(vec![rx_b.as_raw_fd()]);
        write_byte(&tx_a);
        watcher.barrier();
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn a_dead_fd_is_pruned_instead_of_spinning() {
    let (waker, count) = counter();
    let watcher = FdWatcher::new(waker).unwrap();
    let (rx, _tx) = make_pipe();
    let dead = 1 << 20;
    watcher.set_fds(vec![dead, rx.as_raw_fd()]);
    let end = Instant::now() + Duration::from_secs(5);
    while watcher.fds() != vec![rx.as_raw_fd()] {
        assert!(Instant::now() < end, "dead fd never pruned");
        std::thread::sleep(Duration::from_millis(1));
    }
    watcher.barrier();
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn dropping_stops_the_thread_promptly() {
    let (waker, count) = counter();
    let watcher = FdWatcher::new(waker).unwrap();
    let (rx, tx) = make_pipe();
    watcher.set_fds(vec![rx.as_raw_fd()]);
    let start = Instant::now();
    drop(watcher);
    assert!(start.elapsed() < Duration::from_secs(2));
    write_byte(&tx);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
