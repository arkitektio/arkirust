//! The async runtime the agent runs on.
//!
//! The executor, the journal and the state hub spawn tasks, sleep and wait,
//! but name no runtime: they go through the process's [`Runtime`]. With the
//! `tokio` feature that is tokio (the default when none was installed);
//! elsewhere (wasm, a firmware's own executor) the app installs one before it
//! builds an agent:
//!
//! ```ignore
//! rekuest::rt::install(Arc::new(MyRuntime)).ok();
//! ```
//!
//! The primitives here ([`FifoMutex`], [`Latch`], [`Watch`]) need nothing
//! from a runtime: they wake whoever polls them.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

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
        Box::pin(std::future::ready(()))
    }

    /// Whether [`Runtime::spawn`] works from here (tokio: inside a runtime).
    fn is_available(&self) -> bool {
        true
    }
}

static RUNTIME: OnceLock<Arc<dyn Runtime>> = OnceLock::new();

/// Make `runtime` the process's runtime. Only the first call wins; later
/// ones hand their runtime back.
pub fn install(runtime: Arc<dyn Runtime>) -> Result<(), Arc<dyn Runtime>> {
    RUNTIME.set(runtime)
}

/// The installed runtime, else the JS event loop (`web`, on wasm32), else
/// tokio (with the `tokio` feature).
pub fn current() -> &'static dyn Runtime {
    match RUNTIME.get() {
        Some(runtime) => runtime.as_ref(),
        None => fallback()
            .expect("no async runtime: call rekuest::rt::install before building an agent"),
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
    #[cfg(all(feature = "web", target_arch = "wasm32"))]
    return Some(&Js);
    #[cfg(feature = "tokio")]
    return Some(&Tokio);
    None
}

/// The JS event loop (wasm32 with `web`): `spawn_local` and `setTimeout`.
/// Its futures are not `Send`; wasm has one thread, so wrapping them is sound.
#[cfg(all(feature = "web", target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Js;

#[cfg(all(feature = "web", target_arch = "wasm32"))]
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

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// `future`, or [`Elapsed`] once `duration` passed.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    let future = std::pin::pin!(future);
    match futures::future::select(future, current().sleep(duration)).await {
        futures::future::Either::Left((output, _)) => Ok(output),
        futures::future::Either::Right(_) => Err(Elapsed),
    }
}

/// Let other tasks run.
pub async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
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
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .map_err(|panic| panic_message(&*panic));
            let _ = tx.send(result);
        }))
        .await;
    rx.await
        .unwrap_or_else(|_| Err("the blocking work never ran".into()))
}

pub(crate) fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
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
            let mut state = self.state.lock().expect("fifo lock");
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
            let mut state = self.state.lock().expect("fifo lock");
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
        let mut state = self.mutex.state.lock().expect("fifo lock");
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
            let mut state = self.mutex.state.lock().expect("fifo lock");
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
        let wakers = std::mem::take(&mut *self.wakers.lock().expect("latch lock"));
        for waker in wakers {
            waker.wake();
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        std::future::poll_fn(|cx| {
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
        let waker = self.waker.lock().expect("signal lock").take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Wait for a raise (or take one that came before), and lower it.
    pub async fn wait(&self) {
        std::future::poll_fn(|cx| {
            if self.raised.swap(false, Ordering::SeqCst) {
                return Poll::Ready(());
            }
            *self.waker.lock().expect("signal lock") = Some(cx.waker().clone());
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
        *self.value.lock().expect("watch lock") = value;
        let wakers = std::mem::take(&mut *self.wakers.lock().expect("watch lock"));
        for waker in wakers {
            waker.wake();
        }
    }

    /// Wait until `ready(value)` holds.
    pub async fn wait_for(&self, mut ready: impl FnMut(&T) -> bool) {
        std::future::poll_fn(|cx| {
            if ready(&self.value.lock().expect("watch lock")) {
                return Poll::Ready(());
            }
            register(&self.wakers, cx.waker());
            if ready(&self.value.lock().expect("watch lock")) {
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
    let mut wakers = wakers.lock().expect("wakers lock");
    if !wakers.iter().any(|w| w.will_wake(waker)) {
        wakers.push(waker.clone());
    }
}

// --------------------------------------------------------- task locals --

/// A value visible to code polled inside [`TaskLocal::scope`], as tokio's
/// task locals are, on any runtime: it is set while the scoped future is
/// polled, so blocking work it hands to another thread does not see it.
pub struct TaskLocal<T: 'static> {
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<T>>>,
}

impl<T: Clone + 'static> TaskLocal<T> {
    #[doc(hidden)]
    pub const fn new(slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<T>>>) -> Self {
        Self { slot }
    }

    /// Run `future` with `value` set.
    pub async fn scope<F: Future>(&'static self, value: T, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(move |cx| {
            let previous = self.slot.with(|s| s.replace(Some(value.clone())));
            let poll = future.as_mut().poll(cx);
            self.slot.with(|s| *s.borrow_mut() = previous);
            poll
        })
        .await
    }

    /// The value of the scope being polled, if any.
    pub fn get(&'static self) -> Option<T> {
        self.slot.with(|s| s.borrow().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fifo_mutex_serves_in_arrival_order() {
        let mutex = Arc::new(FifoMutex::new());
        let order = Arc::new(Mutex::new(vec![]));
        let first = mutex.clone().lock_owned().await;
        let mut handles = vec![];
        for i in 0..20 {
            let (mutex, order) = (mutex.clone(), order.clone());
            handles.push(tokio::spawn(async move {
                let _guard = mutex.lock_owned().await;
                order.lock().unwrap().push(i);
                yield_now().await;
            }));
            // Each has queued before the next one starts.
            tokio::task::yield_now().await;
        }
        drop(first);
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn a_waiter_that_gives_up_lets_the_next_one_through() {
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
        let latch = Latch::new();
        latch.open();
        timeout(Duration::from_secs(1), latch.wait()).await.unwrap();
    }

    #[tokio::test]
    async fn blocking_work_reports_its_panic() {
        assert_eq!(spawn_blocking(|| 2 + 2).await, Ok(4));
        let err = spawn_blocking(|| -> i32 { panic!("boom") })
            .await
            .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[tokio::test]
    async fn an_aborted_task_stops() {
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
