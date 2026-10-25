//! rekuest's runtime on an embassy executor.
//!
//! Spawns through embassy's spawner (a pool of boxed tasks), sleeps on
//! embassy-time, and keeps the wall clock and randomness the board brings:
//! a hardware RNG, and the time from SNTP or an RTC ([`Embassy::set_wall_clock`]).
//! fakts' [`NetHost`](crate::NetHost) reads both from here too.

use alloc::boxed::Box;
use core::time::Duration;

use arkitekt_spec::rt::{BoxFuture, Runtime};
use embassy_executor::SendSpawner;
use embassy_time::{Instant, Timer};

/// rekuest's tasks: assignments, activation, the journal writer. Each spawn
/// takes one; a full pool panics, so size it for the concurrent tasks.
#[embassy_executor::task(pool_size = 32)]
async fn boxed(future: BoxFuture) {
    future.await
}

/// Random bytes from the board (e.g. esp-hal's `Rng`, an STM32's RNG).
pub trait Entropy: Send + Sync {
    fn fill(&self, bytes: &mut [u8]);
}

/// See the module docs. Install it with `arkitekt_spec::rt::install` before
/// building an agent.
pub struct Embassy {
    spawner: SendSpawner,
    entropy: Box<dyn Entropy>,
    /// Unix microseconds at `Instant` zero; 0 until the clock is set. (Not
    /// an atomic: Cortex-M has no 64-bit ones.)
    boot_epoch_micros: spin::Mutex<i64>,
}

impl Embassy {
    pub fn new(spawner: SendSpawner, entropy: impl Entropy + 'static) -> Self {
        Self {
            spawner,
            entropy: Box::new(entropy),
            boot_epoch_micros: spin::Mutex::new(0),
        }
    }

    /// The wall clock is `unix_micros` now (from SNTP, an RTC, a server's `Date`).
    pub fn set_wall_clock(&self, unix_micros: i64) {
        *self.boot_epoch_micros.lock() = unix_micros - Instant::now().as_micros() as i64;
    }
}

impl Runtime for Embassy {
    fn spawn(&self, future: BoxFuture) {
        self.spawner
            .spawn(boxed(future).expect("arkitekt-embassy's task pool is full"));
    }

    fn sleep(&self, duration: Duration) -> BoxFuture {
        Box::pin(Timer::after(embassy_time::Duration::from_micros(
            duration.as_micros() as u64,
        )))
    }

    fn wall_clock_micros(&self) -> i64 {
        *self.boot_epoch_micros.lock() + Instant::now().as_micros() as i64
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        self.entropy.fill(bytes)
    }
}
