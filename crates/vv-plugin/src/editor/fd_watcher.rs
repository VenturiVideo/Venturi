//! Wakes the app when one of the plugin's fds becomes readable, so the run loop
//! needs no periodic polling. The thread touches raw fds only; the handlers
//! still run on the UI thread, which re-arms the watcher once it pumped.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::Waker;

#[derive(Default)]
struct State {
    fds: Vec<RawFd>,
    generation: u64,
    armed: bool,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    wake_rx: OwnedFd,
    wake_tx: OwnedFd,
    #[cfg(test)]
    seen: std::sync::atomic::AtomicU64,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn poke(&self) {
        // A full pipe already holds a pending wake-up.
        // SAFETY: writes one byte from a valid buffer to our own open pipe.
        unsafe { libc::write(self.wake_tx.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        // SAFETY: reads into a valid buffer from our own non-blocking pipe.
        while unsafe { libc::read(self.wake_rx.as_raw_fd(), buf.as_mut_ptr().cast(), 64) } > 0 {}
    }
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both are fresh descriptors we own.
    let (rx, tx) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&rx, &tx] {
        // SAFETY: flags of a descriptor we own.
        unsafe {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    Ok((rx, tx))
}

pub(super) struct FdWatcher {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl FdWatcher {
    pub fn new(waker: Waker) -> std::io::Result<Self> {
        let (wake_rx, wake_tx) = pipe()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                armed: true,
                ..Default::default()
            }),
            wake_rx,
            wake_tx,
            #[cfg(test)]
            seen: Default::default(),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("vv-plugin-fds".into())
            .spawn(move || run(&worker, &waker))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn set_fds(&self, fds: Vec<RawFd>) {
        let mut state = lock(&self.shared.state);
        state.fds = fds;
        state.generation += 1;
        drop(state);
        self.shared.poke();
    }

    #[cfg(test)]
    fn fds(&self) -> Vec<RawFd> {
        lock(&self.shared.state).fds.clone()
    }

    /// Returns once the watcher has snapshotted every `set_fds` made so far.
    #[cfg(test)]
    fn barrier(&self) {
        use std::sync::atomic::Ordering;
        let generation = {
            let mut state = lock(&self.shared.state);
            state.generation += 1;
            state.generation
        };
        self.shared.poke();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.shared.seen.load(Ordering::SeqCst) < generation {
            assert!(std::time::Instant::now() < end, "watcher never caught up");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The UI thread pumped the handlers: wake again on the next readable fd.
    pub fn rearm(&self) {
        let was_armed = std::mem::replace(&mut lock(&self.shared.state).armed, true);
        if !was_armed {
            self.shared.poke();
        }
    }
}

impl Drop for FdWatcher {
    fn drop(&mut self) {
        lock(&self.shared.state).stop = true;
        self.shared.poke();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const READY: libc::c_short = libc::POLLIN | libc::POLLHUP | libc::POLLERR;

fn run(shared: &Shared, waker: &Waker) {
    loop {
        let (fds, armed, generation) = {
            let state = lock(&shared.state);
            if state.stop {
                return;
            }
            #[cfg(test)]
            shared
                .seen
                .store(state.generation, std::sync::atomic::Ordering::SeqCst);
            (state.fds.clone(), state.armed, state.generation)
        };
        let mut polls = vec![libc::pollfd {
            fd: shared.wake_rx.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        if armed {
            polls.extend(fds.iter().map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            }));
        }
        // SAFETY: `polls` is a valid array of the length passed.
        let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return;
            }
            continue;
        }
        if polls[0].revents != 0 {
            shared.drain();
        }
        let dead: Vec<RawFd> = polls[1..]
            .iter()
            .filter(|p| p.revents & libc::POLLNVAL != 0)
            .map(|p| p.fd)
            .collect();
        let ready = polls[1..].iter().any(|p| p.revents & READY != 0);
        let mut state = lock(&shared.state);
        if state.generation != generation {
            // Stale snapshot: poll again; still-watched ready fds report again.
            continue;
        }
        state.fds.retain(|fd| !dead.contains(fd));
        if ready && !state.stop {
            state.armed = false;
            drop(state);
            waker();
        }
    }
}

#[cfg(test)]
#[path = "../tests/editor/fd_watcher.rs"]
mod tests;
