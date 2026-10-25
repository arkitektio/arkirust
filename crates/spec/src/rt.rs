//! The async runtime the agent runs on.
//!
//! The executor, the journal and the state hub spawn tasks, sleep and wait,
//! but name no runtime: they go through the process's [`Runtime`]. With the
//! `tokio` feature that is tokio (the default when none was installed);
//! elsewhere (wasm, a firmware's own executor) the app installs one before it
//! builds an agent:
//!
//! ```ignore
//! arkitekt_spec::rt::install(Arc::new(MyRuntime)).ok();
//! ```
//!
//! The primitives here ([`FifoMutex`], [`Latch`], [`Watch`]) need nothing
//! from a runtime: they wake whoever polls them.

#[allow(unused_imports)]
use crate::prelude::*;
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};
use core::time::Duration;

use futures::future::{AbortHandle, Abortable};

/// A future the runtime runs to its end.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What rekuest needs from an async runtime.
pub trait Runtime: Send + Sync + 'static {
    /// Run `future` to its end, detached.
    fn spawn(&self, future: BoxFuture);

    /// A future that is ready after `duration`.
    fn sleep(&self, duration: Duration) -> BoxFuture;

    /// Run `work` where blocking is fine (a thread pool), resolving once it
    /// ran. The default runs it right away, on the caller (a single-threaded
    /// target has nowhere else to run it).
    fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>) -> BoxFuture {
        work();
        Box::pin(core::future::ready(()))
    }

    /// Whether [`Runtime::spawn`] works from here (tokio: inside a runtime).
    fn is_available(&self) -> bool {
        true
    }

    /// Microseconds since the Unix epoch (UTC): journal and state
    /// timestamps, `Task::now`. With std the system clock; a runtime without
    /// std (embassy) brings its own, e.g. from SNTP.
    #[cfg(feature = "std")]
    fn wall_clock_micros(&self) -> i64 {
        chrono::Utc::now().timestamp_micros()
    }
    #[cfg(not(feature = "std"))]
    fn wall_clock_micros(&self) -> i64;

    /// Fill `bytes` with random ones: message and task ids, `Task::random`.
    /// With std the OS's; a runtime without std brings its own (a hardware RNG).
    #[cfg(feature = "std")]
    fn fill_random(&self, bytes: &mut [u8]) {
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), bytes)
    }
    #[cfg(not(feature = "std"))]
    fn fill_random(&self, bytes: &mut [u8]);
}

static RUNTIME: spin::Once<Arc<dyn Runtime>> = spin::Once::new();

/// Make `runtime` the process's runtime. Only the first call wins; later
/// ones hand their runtime back.
pub fn install(runtime: Arc<dyn Runtime>) -> Result<(), Arc<dyn Runtime>> {
    let mut runtime = Some(runtime);
    RUNTIME.call_once(|| runtime.take().expect("taken once"));
    match runtime {
        None => Ok(()),
        Some(refused) => Err(refused),
    }
}

/// The installed runtime, else the JS event loop (`web`, on wasm32), else
/// tokio (with the `tokio` feature).
pub fn current() -> &'static dyn Runtime {
    match RUNTIME.get() {
        Some(runtime) => runtime.as_ref(),
        None => fallback()
            .expect("no async runtime: call arkitekt_spec::rt::install (or a platform's setup) before building an agent"),
    }
}

/// Whether there is a runtime to spawn on from here.
pub fn available() -> bool {
    match RUNTIME.get() {
        Some(runtime) => runtime.is_available(),
        None => fallback().is_some_and(|runtime| runtime.is_available()),
    }
}

#[allow(unreachable_code)]
fn fallback() -> Option<&'static dyn Runtime> {
    #[cfg(all(feature = "js", target_arch = "wasm32"))]
    return Some(&Js);
    #[cfg(feature = "tokio")]
    return Some(&Tokio);
    None
}

/// The JS event loop (wasm32 with `web`): `spawn_local` and `setTimeout`.
/// Its futures are not `Send`; wasm has one thread, so wrapping them is sound.
#[cfg(all(feature = "js", target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Js;

#[cfg(all(feature = "js", target_arch = "wasm32"))]
impl Runtime for Js {
    fn spawn(&self, future: BoxFuture) {
        wasm_bindgen_futures::spawn_local(future);
    }

    fn sleep(&self, duration: Duration) -> BoxFuture {
        let millis = duration.as_millis().min(u32::MAX as u128) as u32;
        Box::pin(send_wrapper::SendWrapper::new(
            gloo_timers::future::TimeoutFuture::new(millis),
        ))
    }
}

/// tokio, found at every call (each `#[tokio::test]` has a runtime of its own).
#[cfg(feature = "tokio")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokio;

#[cfg(feature = "tokio")]
impl Runtime for Tokio {
    fn spawn(&self, future: BoxFuture) {
        tokio::spawn(future);
    }

    fn sleep(&self, duration: Duration) -> BoxFuture {
        Box::pin(tokio::time::sleep(duration))
    }

    fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>) -> BoxFuture {
        let handle = tokio::task::spawn_blocking(work);
        Box::pin(async move {
            let _ = handle.await;
        })
    }

    fn is_available(&self) -> bool {
        tokio::runtime::Handle::try_current().is_ok()
    }
}

/// A spawned task; [`TaskHandle::abort`] drops it at its next poll.
#[derive(Debug, Clone)]
pub struct TaskHandle(AbortHandle);

impl TaskHandle {
    pub fn abort(&self) {
        self.0.abort()
    }
}

/// Spawn `future` on the runtime.
pub fn spawn(future: impl Future<Output = ()> + Send + 'static) -> TaskHandle {
    let (handle, registration) = AbortHandle::new_pair();
    current().spawn(Box::pin(async move {
        let _ = Abortable::new(future, registration).await;
    }));
    TaskHandle(handle)
}

pub async fn sleep(duration: Duration) {
    current().sleep(duration).await
}

/// `future` did not finish in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl core::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("deadline elapsed")
    }
}

impl core::error::Error for Elapsed {}

/// `future`, or [`Elapsed`] once `duration` passed.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    let future = core::pin::pin!(future);
    match futures::future::select(future, current().sleep(duration)).await {
        futures::future::Either::Left((output, _)) => Ok(output),
        futures::future::Either::Right(_) => Err(Elapsed),
    }
}

/// Let other tasks run.
pub async fn yield_now() {
    let mut yielded = false;
    core::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

/// Run blocking `work` through the runtime; `Err` carries a panic's message.
pub async fn spawn_blocking<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    let (tx, rx) = futures::channel::oneshot::channel();
    current()
        .spawn_blocking(Box::new(move || {
            #[cfg(feature = "std")]
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .map_err(|panic| panic_message(&*panic));
            // Without std a panic aborts: there is nothing to catch.
            #[cfg(not(feature = "std"))]
            let result: Result<R, String> = Ok(work());
            let _ = tx.send(result);
        }))
        .await;
    rx.await
        .unwrap_or_else(|_| Err("the blocking work never ran".into()))
}

#[cfg(feature = "std")]
#[doc(hidden)]
pub fn panic_message(panic: &(dyn core::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

// ------------------------------------------------------- clock, randomness --

/// The wall clock (UTC): std's, or the runtime's without std.
pub fn now() -> chrono::DateTime<chrono::Utc> {
    #[cfg(feature = "std")]
    return chrono::Utc::now();
    #[cfg(not(feature = "std"))]
    chrono::DateTime::from_timestamp_micros(current().wall_clock_micros()).unwrap_or_default()
}

/// Random bytes: the OS's with std, the runtime's without.
pub fn fill_random(bytes: &mut [u8]) {
    #[cfg(feature = "std")]
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), bytes);
    #[cfg(not(feature = "std"))]
    current().fill_random(bytes);
}

/// A random (v4) uuid.
pub fn uuid() -> uuid::Uuid {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

// ------------------------------------------------------------- primitives --

/// An async mutex that hands the lock out strictly first come, first served
/// (named locks and per-action serialization promise FIFO).
#[derive(Default)]
pub struct FifoMutex {
    state: Mutex<FifoState>,
}

#[derive(Default)]
struct FifoState {
    locked: bool,
    next_ticket: u64,
    /// Waiters in arrival order: (ticket, waker).
    queue: VecDeque<(u64, Option<Waker>)>,
}

/// Holds a [`FifoMutex`] until dropped.
pub struct FifoGuard {
    mutex: Arc<FifoMutex>,
}

impl FifoMutex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wait for the lock, behind everyone who asked before.
    pub async fn lock_owned(self: Arc<Self>) -> FifoGuard {
        let ticket = {
            let mut state = self.state.lock();
            if !state.locked && state.queue.is_empty() {
                state.locked = true;
                None
            } else {
                let ticket = state.next_ticket;
                state.next_ticket += 1;
                state.queue.push_back((ticket, None));
                Some(ticket)
            }
        };
        let Some(ticket) = ticket else {
            return FifoGuard { mutex: self };
        };
        let waiting = Waiting {
            mutex: self.clone(),
            ticket,
            done: false,
        };
        waiting.await;
        FifoGuard { mutex: self }
    }

    fn unlock(&self) {
        let next = {
            let mut state = self.state.lock();
            state.locked = false;
            state.queue.front().and_then(|(_, waker)| waker.clone())
        };
        // Woken outside the lock: an executor may poll right inside `wake`.
        if let Some(waker) = next {
            waker.wake();
        }
    }
}

/// Waiting for a turn; dropped early, it leaves the queue.
struct Waiting {
    mutex: Arc<FifoMutex>,
    ticket: u64,
    done: bool,
}

impl Future for Waiting {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.mutex.state.lock();
        let first = state.queue.front().map(|(t, _)| *t) == Some(self.ticket);
        if first && !state.locked {
            state.queue.pop_front();
            state.locked = true;
            drop(state);
            self.done = true;
            return Poll::Ready(());
        }
        if let Some(entry) = state.queue.iter_mut().find(|(t, _)| *t == self.ticket) {
            entry.1 = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let next = {
            let mut state = self.mutex.state.lock();
            let was_first = state.queue.front().map(|(t, _)| *t) == Some(self.ticket);
            state.queue.retain(|(t, _)| *t != self.ticket);
            // The next in line may be free to go now.
            (was_first && !state.locked)
                .then(|| state.queue.front().and_then(|(_, waker)| waker.clone()))
                .flatten()
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }
}

impl Drop for FifoGuard {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

/// Opens once and stays open; everyone waiting (or arriving later) passes.
#[derive(Default)]
pub struct Latch {
    open: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl Latch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        let wakers = core::mem::take(&mut *self.wakers.lock());
        for waker in wakers {
            waker.wake();
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        core::future::poll_fn(|cx| {
            if self.is_open() {
                return Poll::Ready(());
            }
            register(&self.wakers, cx.waker());
            // Opened between the check and registering: do not miss it.
            if self.is_open() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

/// Wakes one waiter, and remembers a wake-up nobody waited for (as tokio's
/// `Notify::notify_one`): the next [`Signal::wait`] passes at once.
#[derive(Default)]
pub struct Signal {
    raised: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Signal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn raise(&self) {
        self.raised.store(true, Ordering::SeqCst);
        let waker = self.waker.lock().take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Wait for a raise (or take one that came before), and lower it.
    pub async fn wait(&self) {
        core::future::poll_fn(|cx| {
            if self.raised.swap(false, Ordering::SeqCst) {
                return Poll::Ready(());
            }
            *self.waker.lock() = Some(cx.waker().clone());
            if self.raised.swap(false, Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

/// A value others can wait on until it satisfies them.
pub struct Watch<T> {
    value: Mutex<T>,
    wakers: Mutex<Vec<Waker>>,
}

impl<T> Watch<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
            wakers: Mutex::new(vec![]),
        }
    }

    pub fn set(&self, value: T) {
        *self.value.lock() = value;
        let wakers = core::mem::take(&mut *self.wakers.lock());
        for waker in wakers {
            waker.wake();
        }
    }

    /// Wait until `ready(value)` holds.
    pub async fn wait_for(&self, mut ready: impl FnMut(&T) -> bool) {
        core::future::poll_fn(|cx| {
            if ready(&self.value.lock()) {
                return Poll::Ready(());
            }
            register(&self.wakers, cx.waker());
            if ready(&self.value.lock()) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

/// Remember `waker` once (a task polled again keeps one entry).
fn register(wakers: &Mutex<Vec<Waker>>, waker: &Waker) {
    let mut wakers = wakers.lock();
    if !wakers.iter().any(|w| w.will_wake(waker)) {
        wakers.push(waker.clone());
    }
}

// --------------------------------------------------------- task locals --

/// A value visible to code polled inside [`TaskLocal::scope`], as tokio's
/// task locals are, on any runtime: it is set while the scoped future is
/// polled, so blocking work it hands to another thread does not see it.
///
/// With std it lives in a thread local; without std (one executor thread, as
/// embassy runs) in a global slot.
pub struct TaskLocal<T: 'static> {
    #[cfg(feature = "std")]
    slot: &'static std::thread::LocalKey<core::cell::RefCell<Option<T>>>,
    #[cfg(not(feature = "std"))]
    slot: spin::Mutex<Option<T>>,
}

#[cfg(not(feature = "std"))]
impl<T: Clone + 'static> Default for TaskLocal<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + 'static> TaskLocal<T> {
    #[cfg(feature = "std")]
    pub const fn new(slot: &'static std::thread::LocalKey<core::cell::RefCell<Option<T>>>) -> Self {
        Self { slot }
    }

    #[cfg(not(feature = "std"))]
    pub const fn new() -> Self {
        Self {
            slot: spin::Mutex::new(None),
        }
    }

    fn replace(&'static self, value: Option<T>) -> Option<T> {
        #[cfg(feature = "std")]
        return self.slot.with(|s| s.replace(value));
        #[cfg(not(feature = "std"))]
        core::mem::replace(&mut *self.slot.lock(), value)
    }

    /// Run `future` with `value` set.
    pub async fn scope<F: Future>(&'static self, value: T, future: F) -> F::Output {
        let mut future = core::pin::pin!(future);
        core::future::poll_fn(move |cx| {
            let previous = self.replace(Some(value.clone()));
            let poll = future.as_mut().poll(cx);
            self.replace(previous);
            poll
        })
        .await
    }

    /// The value of the scope being polled, if any.
    pub fn get(&'static self) -> Option<T> {
        #[cfg(feature = "std")]
        return self.slot.with(|s| s.borrow().clone());
        #[cfg(not(feature = "std"))]
        self.slot.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tokio, installed for these tests (the crate's own `Tokio` needs its feature).
    struct TestTokio;

    impl Runtime for TestTokio {
        fn spawn(&self, future: BoxFuture) {
            tokio::spawn(future);
        }

        fn sleep(&self, duration: Duration) -> BoxFuture {
            Box::pin(tokio::time::sleep(duration))
        }

        fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>) -> BoxFuture {
            let handle = tokio::task::spawn_blocking(work);
            Box::pin(async move {
                let _ = handle.await;
            })
        }
    }

    fn tokio_runtime() {
        let _ = install(Arc::new(TestTokio));
    }

    #[tokio::test]
    async fn the_fifo_mutex_serves_in_arrival_order() {
        tokio_runtime();
        let mutex = Arc::new(FifoMutex::new());
        let order = Arc::new(Mutex::new(vec![]));
        let first = mutex.clone().lock_owned().await;
        let mut handles = vec![];
        for i in 0..20 {
            let (mutex, order) = (mutex.clone(), order.clone());
            handles.push(tokio::spawn(async move {
                let _guard = mutex.lock_owned().await;
                order.lock().push(i);
                yield_now().await;
            }));
            // Each has queued before the next one starts.
            tokio::task::yield_now().await;
        }
        drop(first);
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(*order.lock(), (0..20).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn a_waiter_that_gives_up_lets_the_next_one_through() {
        tokio_runtime();
        let mutex = Arc::new(FifoMutex::new());
        let held = mutex.clone().lock_owned().await;
        assert!(
            timeout(Duration::from_millis(20), mutex.clone().lock_owned())
                .await
                .is_err()
        );
        let next = tokio::spawn({
            let mutex = mutex.clone();
            async move { drop(mutex.lock_owned().await) }
        });
        tokio::task::yield_now().await;
        drop(held);
        timeout(Duration::from_secs(1), next)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_latch_opened_before_the_wait_lets_it_pass() {
        tokio_runtime();
        let latch = Latch::new();
        latch.open();
        timeout(Duration::from_secs(1), latch.wait()).await.unwrap();
    }

    #[tokio::test]
    async fn blocking_work_reports_its_panic() {
        tokio_runtime();
        assert_eq!(spawn_blocking(|| 2 + 2).await, Ok(4));
        let err = spawn_blocking(|| -> i32 { panic!("boom") })
            .await
            .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[tokio::test]
    async fn an_aborted_task_stops() {
        tokio_runtime();
        let latch = Arc::new(Latch::new());
        let handle = spawn({
            let latch = latch.clone();
            async move {
                sleep(Duration::from_secs(3600)).await;
                latch.open();
            }
        });
        handle.abort();
        sleep(Duration::from_millis(20)).await;
        assert!(!latch.is_open());
    }
}
