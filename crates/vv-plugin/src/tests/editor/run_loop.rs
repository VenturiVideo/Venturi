use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, ITimerHandler, ITimerHandlerTrait,
};
use vst3::{Class, ComPtr, ComWrapper};

use super::*;

/// Unregisters itself from its destructor, as a plugin releasing its own state may.
struct Timers {
    run_loop: Rc<RunLoop>,
    me: Rc<Cell<usize>>,
}

impl Class for Timers {
    type Interfaces = (ITimerHandler,);
}

impl ITimerHandlerTrait for Timers {
    unsafe fn onTimer(&self) {}
}

impl Drop for Timers {
    fn drop(&mut self) {
        self.run_loop
            .remove_timer(self.me.get() as *mut ITimerHandler);
    }
}

struct Events {
    run_loop: Rc<RunLoop>,
    me: Rc<Cell<usize>>,
}

impl Class for Events {
    type Interfaces = (IEventHandler,);
}

impl IEventHandlerTrait for Events {
    unsafe fn onFDIsSet(&self, _: FileDescriptor) {}
}

impl Drop for Events {
    fn drop(&mut self) {
        self.run_loop
            .remove_handler(self.me.get() as *mut IEventHandler);
    }
}

fn timer(run_loop: &Rc<RunLoop>) -> ComPtr<ITimerHandler> {
    let me = Rc::new(Cell::new(0));
    let ptr = ComWrapper::new(Timers {
        run_loop: run_loop.clone(),
        me: me.clone(),
    })
    .to_com_ptr::<ITimerHandler>()
    .unwrap();
    me.set(ptr.as_ptr() as usize);
    ptr
}

fn event(run_loop: &Rc<RunLoop>) -> ComPtr<IEventHandler> {
    let me = Rc::new(Cell::new(0));
    let ptr = ComWrapper::new(Events {
        run_loop: run_loop.clone(),
        me: me.clone(),
    })
    .to_com_ptr::<IEventHandler>()
    .unwrap();
    me.set(ptr.as_ptr() as usize);
    ptr
}

const TICK: Duration = Duration::from_millis(5);

/// Counts its own releases, so a test can see the run loop let go of it.
#[cfg(target_os = "linux")]
struct Released(Rc<Cell<usize>>);

#[cfg(target_os = "linux")]
impl Class for Released {
    type Interfaces = (IEventHandler,);
}

#[cfg(target_os = "linux")]
impl IEventHandlerTrait for Released {
    unsafe fn onFDIsSet(&self, _: FileDescriptor) {}
}

#[cfg(target_os = "linux")]
impl Drop for Released {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn clear_releases_handlers_and_stops_the_watcher() {
    let released = Rc::new(Cell::new(0));
    let rl = Rc::new(RunLoop::new(std::sync::Arc::new(|| {}), None));
    let handler = ComWrapper::new(Released(released.clone()))
        .to_com_ptr::<IEventHandler>()
        .unwrap();
    assert_eq!(rl.add_handler(handler, 3), kResultOk);
    rl.clear();
    assert_eq!(released.get(), 1);
    assert!(rl.handlers().is_empty());
    assert!(rl.watcher.borrow().is_none());
}

#[test]
fn clear_survives_handlers_that_unregister_in_their_destructor() {
    let rl = Rc::new(RunLoop::default());
    // Only the host list owns them once the locals are gone.
    assert_eq!(rl.add_timer(timer(&rl), TICK), kResultOk);
    assert_eq!(rl.add_handler(event(&rl), 3), kResultOk);
    rl.clear();
    assert!(rl.next_wake().is_none());
    assert!(rl.handlers().is_empty());
    assert!(
        rl.take_due(Instant::now() + Duration::from_secs(1))
            .is_empty()
    );
}

#[test]
fn unregister_survives_handlers_that_unregister_in_their_destructor() {
    let rl = Rc::new(RunLoop::default());
    let (gone, kept) = (timer(&rl), timer(&rl));
    let (gone_ptr, kept_ptr) = (gone.as_ptr(), kept.as_ptr());
    rl.add_timer(gone, TICK);
    rl.add_timer(kept, TICK);
    assert_eq!(rl.remove_timer(gone_ptr), kResultOk);
    let due = rl.take_due(Instant::now() + Duration::from_secs(1));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].as_ptr(), kept_ptr);
    drop(due);

    let (gone, kept) = (event(&rl), event(&rl));
    let (gone_ptr, kept_ptr) = (gone.as_ptr(), kept.as_ptr());
    rl.add_handler(gone, 1);
    rl.add_handler(kept, 2);
    assert_eq!(rl.remove_handler(gone_ptr), kResultOk);
    let left = rl.handlers();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].0.as_ptr(), kept_ptr);
    drop(left);
    rl.clear();
}

#[test]
fn a_reentrant_mutation_fails_instead_of_panicking() {
    let rl = Rc::new(RunLoop::default());
    let _held = rl.timers.borrow_mut();
    assert_eq!(rl.add_timer(timer(&rl), TICK), kResultFalse);
    assert_eq!(rl.remove_timer(std::ptr::null_mut()), kResultFalse);
}

#[test]
fn a_registered_fd_does_not_ask_for_polling() {
    let rl = Rc::new(RunLoop::default());
    rl.add_handler(event(&rl), 3);
    assert!(rl.next_wake().is_none());
    rl.add_timer(timer(&rl), TICK);
    assert!(rl.next_wake().is_some_and(|d| d <= TICK));
    rl.clear();
}

#[test]
fn a_registered_fd_wakes_the_app_until_the_pump_rearms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let rl = Rc::new(RunLoop::new(
        Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }),
        None,
    ));
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    rl.add_handler(event(&rl), fds[0]);
    assert_eq!(unsafe { libc::write(fds[1], [1u8].as_ptr().cast(), 1) }, 1);
    let end = Instant::now() + Duration::from_secs(5);
    while count.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    rl.rearm();
    while count.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    rl.clear();
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn without_a_watcher_thread_fds_are_polled() {
    let rl = Rc::new(RunLoop {
        poll_fallback: true,
        ..Default::default()
    });
    assert!(rl.next_wake().is_none());
    rl.add_handler(event(&rl), 3);
    assert!(rl.next_wake().is_some());
    rl.clear();
}

#[test]
fn wake_after_is_the_earliest_of_the_timer_and_the_fd_poll() {
    let now = Instant::now();
    let in_ms = |ms| Some(now + Duration::from_millis(ms));
    assert_eq!(wake_after(None, false, now), None);
    assert_eq!(wake_after(None, true, now), Some(FD_POLL));
    assert_eq!(
        wake_after(in_ms(5), true, now),
        Some(Duration::from_millis(5))
    );
    assert_eq!(wake_after(in_ms(500), true, now), Some(FD_POLL));
    assert_eq!(
        wake_after(in_ms(500), false, now),
        Some(Duration::from_millis(500))
    );
    assert_eq!(
        wake_after(now.checked_sub(FD_POLL), true, now),
        Some(Duration::ZERO)
    );
}

#[test]
fn without_a_watcher_thread_the_extra_fd_is_polled() {
    let rl = Rc::new(RunLoop {
        poll_fallback: true,
        extra_fd: Some(3),
        ..Default::default()
    });
    assert_eq!(rl.next_wake(), Some(FD_POLL));
    rl.add_timer(timer(&rl), Duration::from_millis(1));
    assert!(rl.next_wake().unwrap() <= Duration::from_millis(1));
    rl.clear();
}
