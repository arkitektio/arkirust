//! A minimal app: two actions, no services.
//!
//! ```sh
//! cargo run -p arkitekt-examples --bin hello
//! ```

use std::time::Duration;

use arkitekt::{action, run, App, Task};
use futures::Stream;

/// Greet someone
///
/// Says hello, possibly several times.
///
/// # Arguments
/// * `name` - Who to greet
/// * `times` - How often
///
/// # Returns
/// The greeting
#[action]
async fn greet(name: String, #[port(default = 1)] times: i64) -> String {
    format!("Hello {name}! ")
        .repeat(times.max(1) as usize)
        .trim_end()
        .to_owned()
}

/// Count
///
/// Yields every number up to `to`, one per second.
///
/// # Arguments
/// * `to` - Where to stop (exclusive)
#[action]
async fn count(to: i64, task: Task) -> impl Stream<Item = i64> {
    futures::stream::unfold(0, move |i| {
        let task = task.clone();
        async move {
            if i >= to {
                return None;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            task.progress((100 * (i + 1) / to.max(1)) as i32, format!("at {i}"));
            Some((i, i + 1))
        }
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = App::new("hello-rust", "0.1.0")
        .description("Says hello from Rust")
        .action(greet)
        .action(count);

    run(app).await
}
