//! A real `#[action]` on embassy: rekuest's [`Driver`] (session, executor,
//! journal) on an embassy [`Runtime`], rekuest without std.
//!
//! The runtime spawns through embassy's spawner (a pool of boxed tasks) and
//! sleeps on embassy-time. A board brings the wall clock (SNTP, an RTC) and
//! randomness (its hardware RNG); here they are the boot time plus uptime and
//! a seeded generator.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::time::Duration;

use embassy_executor::SendSpawner;
use embassy_futures::select::{select3, Either3};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Instant, Timer};
use rekuest::driver::Driver;
use rekuest::rt::{BoxFuture, Runtime};
use rekuest::{action, Context, Registry};
use rekuest_session::testing::{FakeServer, ServerOutput};
use rekuest_session::{ConnectionPolicy, Event, Outbox, SessionConfig, SessionError};

/// 2026-10-24T00:00:00Z: where the clock starts (a board asks SNTP).
const BOOT_EPOCH_MICROS: i64 = 1_792_800_000_000_000;

#[embassy_executor::task(pool_size = 16)]
async fn boxed(future: BoxFuture) {
    future.await
}

/// rekuest's runtime on an embassy executor.
pub struct Embassy {
    spawner: SendSpawner,
    rng: spin::Mutex<u64>,
}

impl Embassy {
    pub fn new(spawner: SendSpawner, seed: u64) -> Self {
        Self {
            spawner,
            rng: spin::Mutex::new(seed),
        }
    }
}

impl Runtime for Embassy {
    fn spawn(&self, future: BoxFuture) {
        self.spawner
            .spawn(boxed(future).expect("rekuest's task pool is full"));
    }

    fn sleep(&self, duration: Duration) -> BoxFuture {
        Box::pin(Timer::after(embassy_time::Duration::from_micros(
            duration.as_micros() as u64,
        )))
    }

    fn wall_clock_micros(&self) -> i64 {
        BOOT_EPOCH_MICROS + Instant::now().as_micros() as i64
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        let mut state = self.rng.lock();
        for chunk in bytes.chunks_mut(8) {
            // splitmix64
            *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

/// Hand `x` back (a sync action: the runtime runs it inline)
#[action]
fn echo(x: i64) -> i64 {
    x
}

/// What the fake socket hands the agent.
enum Incoming {
    Frame(alloc::string::String),
    Dropped,
}

/// The scripted scenario with the real agent: executor, journal, `#[action]`.
pub async fn run_agent(spawner: SendSpawner, seed: u64) -> Result<FakeServer, SessionError> {
    let _ = rekuest::rt::install(Arc::new(Embassy::new(spawner, seed)));
    let start = Instant::now();
    let now = || Duration::from_micros(start.elapsed().as_micros());
    let socket: Channel<NoopRawMutex, Incoming, 16> = Channel::new();
    let mut server = FakeServer::scripted();
    let mut registry = Registry::new();
    registry.register(echo);
    let driver = Driver::new(
        registry,
        Context::default(),
        SessionConfig {
            policy: ConnectionPolicy {
                initial: Duration::from_millis(10),
                ..Default::default()
            },
            ..SessionConfig::new("s", seed)
        },
        Outbox::new(),
        None,
    );
    let mut open = false;

    loop {
        let polled = driver.poll(now());
        let busy = !polled.events.is_empty() || !polled.frames.is_empty();
        for frame in &polled.frames {
            if open {
                server.receive(frame);
            }
        }
        for event in polled.events {
            match driver.act(event) {
                Some(Event::Connect) => {
                    server.connected();
                    open = true;
                    let declaration = driver.declaration(Some("embassy-control".into()), None);
                    driver.with_session(|s| s.connected(now(), "token".into(), declaration));
                }
                Some(Event::Close) => {
                    open = false;
                    socket.clear();
                }
                Some(Event::ReloadJournal) => driver.with_session(|s| s.reloaded(None)),
                Some(Event::Ended(error)) => return Err(error),
                _ => {}
            }
        }
        while let Some(output) = server.poll() {
            let incoming = match output {
                ServerOutput::Frame(text) => Incoming::Frame(text),
                ServerOutput::Drop => Incoming::Dropped,
            };
            if socket.try_send(incoming).is_err() {
                panic!("the fake socket is full");
            }
        }
        if server.done() {
            return Ok(server);
        }
        if busy {
            continue;
        }

        let deadline = match polled.deadline {
            Some(at) => start + embassy_time::Duration::from_micros(at.as_micros() as u64),
            None => Instant::MAX,
        };
        match select3(socket.receive(), Timer::at(deadline), driver.woken()).await {
            Either3::First(Incoming::Frame(text)) if open => {
                driver.with_session(|s| s.handle_text(now(), &text))
            }
            Either3::First(Incoming::Frame(_)) => {}
            Either3::First(Incoming::Dropped) => {
                open = false;
                driver.with_session(|s| s.handle_disconnect(now(), "dropped"));
            }
            Either3::Second(()) | Either3::Third(()) => {}
        }
    }
}
