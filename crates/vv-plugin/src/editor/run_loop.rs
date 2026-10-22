//! The handler and timer lists behind the Linux `IRunLoop`. Platform-neutral so
//! the bookkeeping is testable without a display.
use std::cell::RefCell;
use std::ffi::c_int;
use std::time::{Duration, Instant};

use vst3::ComPtr;
use vst3::Steinberg::Linux::{IEventHandler, ITimerHandler};
use vst3::Steinberg::{kResultFalse, kResultOk, tresult};

use super::fd_watcher::FdWatcher;
use crate::Waker;

struct Timer {
    handler: ComPtr<ITimerHandler>,
    interval: Duration,
    next: Instant,
}

/// Used only when no watcher thread could be started.
const FD_POLL: Duration = Duration::from_millis(16);

/// The earliest of the next timer deadline and, when fds need polling, `now + FD_POLL`.
fn wake_after(timer: Option<Instant>, poll_fds: bool, now: Instant) -> Option<Duration> {
    let timer = timer.map(|t| t.saturating_duration_since(now));
    let poll = poll_fds.then_some(FD_POLL);
    match (timer, poll) {
        (Some(t), Some(p)) => Some(t.min(p)),
        (t, p) => t.or(p),
    }
}

/// What the plugin asked of the host's event loop.
///
/// Dropping a handler may run the plugin's destructor, which can unregister
/// itself: entries are therefore released only after the borrow is gone.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Default)]
pub(super) struct RunLoop {
    handlers: RefCell<Vec<(ComPtr<IEventHandler>, c_int)>>,
    timers: RefCell<Vec<Timer>>,
    watcher: RefCell<Option<FdWatcher>>,
    extra_fd: Option<c_int>,
    poll_fallback: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl RunLoop {
    /// Wakes `waker` when a registered fd, or `extra_fd`, becomes readable.
    pub fn new(waker: Waker, extra_fd: Option<c_int>) -> Self {
        let watcher = FdWatcher::new(waker).ok();
        if let Some(w) = &watcher {
            w.set_fds(extra_fd.into_iter().collect());
        }
        Self {
            poll_fallback: watcher.is_none(),
            watcher: RefCell::new(watcher),
            extra_fd,
            ..Default::default()
        }
    }

    fn sync_fds(&self) {
        let Ok(watcher) = self.watcher.try_borrow() else {
            return;
        };
        if let Some(watcher) = watcher.as_ref() {
            let handlers = self.handlers.try_borrow();
            let fds = handlers.iter().flat_map(|h| h.iter().map(|(_, fd)| *fd));
            watcher.set_fds(self.extra_fd.into_iter().chain(fds).collect());
        }
    }

    /// The UI thread pumped: the watcher may wake the app again.
    pub fn rearm(&self) {
        if let Ok(Some(watcher)) = self.watcher.try_borrow().as_deref() {
            watcher.rearm();
        }
    }

    pub fn clear(&self) {
        // Joins the watcher thread.
        drop(self.watcher.try_borrow_mut().map(|mut w| w.take()));
        let handlers = self
            .handlers
            .try_borrow_mut()
            .map(|mut h| std::mem::take(&mut *h));
        let timers = self
            .timers
            .try_borrow_mut()
            .map(|mut t| std::mem::take(&mut *t));
        drop((handlers, timers));
    }

    pub fn add_handler(&self, handler: ComPtr<IEventHandler>, fd: c_int) -> tresult {
        match self.handlers.try_borrow_mut() {
            Ok(mut list) => {
                list.push((handler, fd));
                drop(list);
                self.sync_fds();
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }

    pub fn remove_handler(&self, handler: *mut IEventHandler) -> tresult {
        let removed = match self.handlers.try_borrow_mut() {
            Ok(mut list) => {
                let (gone, kept) = std::mem::take(&mut *list)
                    .into_iter()
                    .partition(|(h, _)| h.as_ptr() == handler);
                *list = kept;
                gone
            }
            Err(_) => return kResultFalse,
        };
        self.sync_fds();
        drop::<Vec<_>>(removed);
        kResultOk
    }

    pub fn add_timer(&self, handler: ComPtr<ITimerHandler>, interval: Duration) -> tresult {
        match self.timers.try_borrow_mut() {
            Ok(mut list) => {
                list.push(Timer {
                    handler,
                    interval,
                    next: Instant::now() + interval,
                });
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }

    pub fn remove_timer(&self, handler: *mut ITimerHandler) -> tresult {
        let removed = match self.timers.try_borrow_mut() {
            Ok(mut list) => {
                let (gone, kept) = std::mem::take(&mut *list)
                    .into_iter()
                    .partition(|t| t.handler.as_ptr() == handler);
                *list = kept;
                gone
            }
            Err(_) => return kResultFalse,
        };
        drop::<Vec<_>>(removed);
        kResultOk
    }

    /// The next timer deadline; fds wake the app through the watcher (polled if it failed).
    pub fn next_wake(&self) -> Option<Duration> {
        let poll_fds = self.poll_fallback
            && (self.extra_fd.is_some() || !self.handlers.try_borrow().ok()?.is_empty());
        let next = self.timers.try_borrow().ok()?.iter().map(|t| t.next).min();
        wake_after(next, poll_fds, Instant::now())
    }

    /// Clones, so the callbacks run with no borrow held.
    pub fn handlers(&self) -> Vec<(ComPtr<IEventHandler>, c_int)> {
        self.handlers
            .try_borrow()
            .map(|h| h.clone())
            .unwrap_or_default()
    }

    pub fn has_handler(&self, handler: &ComPtr<IEventHandler>) -> bool {
        self.handlers
            .try_borrow()
            .is_ok_and(|h| h.iter().any(|(h, _)| h.as_ptr() == handler.as_ptr()))
    }

    /// The timers due at `now`, rescheduled.
    pub fn take_due(&self, now: Instant) -> Vec<ComPtr<ITimerHandler>> {
        let Ok(mut timers) = self.timers.try_borrow_mut() else {
            return Vec::new();
        };
        timers
            .iter_mut()
            .filter(|t| t.next <= now)
            .map(|t| {
                t.next = now.checked_add(t.interval).unwrap_or(now);
                t.handler.clone()
            })
            .collect()
    }

    pub fn has_timer(&self, handler: &ComPtr<ITimerHandler>) -> bool {
        self.timers
            .try_borrow()
            .is_ok_and(|t| t.iter().any(|t| t.handler.as_ptr() == handler.as_ptr()))
    }
}

#[cfg(test)]
#[path = "../tests/editor/run_loop.rs"]
mod tests;
