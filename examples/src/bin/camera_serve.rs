//! Serving: an app with state, served over HTTP without a rekuest server.
//!
//! Run:  cargo run -p arkitekt-examples --bin camera_serve
//!
//! Like Python's `arkitekt.serve(app, fastapi_app)`: the actions become REST
//! commands next to your own routes, and everything the app does streams over
//! the websocket at `/ws`. Try:
//!
//! ```sh
//! curl -X POST localhost:8099/set_exposure -H 'content-type: application/json' -d '{"args": {"exposure_ms": 25}}'
//! curl localhost:8099/states/CameraState       # the state, as the app tracks it
//! open http://localhost:8099/docs
//! ```
//!
//! There are no "getter" actions here on purpose. The served app already
//! keeps track of every state (`GET /states`, `STATE_PATCH` frames), so
//! actions are only for doing something.

use std::time::Duration;

use arkitekt::{action, serve, App, ServeOptions, State, StateMut, Task};
use axum::routing::get;
use serde::{Deserialize, Serialize};

/// What the (simulated) camera is doing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, State)]
#[state(name = "CameraState", locks = ["camera"])]
struct CameraState {
    /// Whether the camera is connected
    connected: bool,
    /// Exposure time in milliseconds
    exposure_ms: f64,
    /// Frames acquired so far
    frames: i64,
}

/// What the camera's sensor reports. Needs no lock, so a background worker may change it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, State)]
#[state(name = "SensorState")]
struct SensorState {
    /// Sensor temperature in °C
    temperature: f64,
}

/// Set Exposure
///
/// Changes the exposure time.
///
/// # Arguments
/// * `exposure_ms` - The new exposure time in milliseconds
#[action]
async fn set_exposure(exposure_ms: f64, camera: StateMut<CameraState>) -> anyhow::Result<f64> {
    anyhow::ensure!(exposure_ms > 0.0, "the exposure must be positive");
    camera.update(|c| c.exposure_ms = exposure_ms)?;
    Ok(exposure_ms)
}

/// Acquire
///
/// Acquires frames, one per exposure. Can be paused between frames.
///
/// # Arguments
/// * `count` - How many frames to acquire
#[action]
async fn acquire(#[port(default = 3)] count: i64, camera: StateMut<CameraState>, task: Task) -> anyhow::Result<i64> {
    for i in 0..count {
        task.pausepoint().await;
        let exposure = camera.read(|c| c.exposure_ms);
        tokio::time::sleep(Duration::from_millis(exposure as u64)).await;
        camera.update(|c| c.frames += 1)?;
        task.progress((100 * (i + 1) / count.max(1)) as i32, format!("frame {}", i + 1));
    }
    Ok(camera.read(|c| c.frames))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let app = App::new("camera", "0.1.0")
        .description("A simulated camera, served over HTTP")
        .state(CameraState::default())
        .state(SensorState { temperature: 20.0 })
        .startup(|startup| async move {
            // Connect the hardware here; its initial state becomes the session's baseline.
            startup.set_state(CameraState {
                connected: true,
                exposure_ms: 10.0,
                frames: 0,
            })?;
            Ok(())
        })
        .background(|background| async move {
            // Runs for the app's lifetime; every change is published as a STATE_PATCH.
            let sensor = background.state::<SensorState>()?;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                sensor.update(|s| s.temperature = 20.0 + rand::random::<f64>())?;
            }
        })
        .shutdown(|_| async move {
            tracing::info!("disconnecting the camera");
            Ok(())
        })
        .action(set_exposure)
        .action(acquire);

    // Nothing above connects: an App is a declaration. `serve` runs the startup
    // hooks and mounts the agent next to your own routes.
    let router = axum::Router::new().route("/health", get(|| async { "ok" }));
    serve(app, router, ServeOptions::default()).await?.listen("0.0.0.0:8099").await
}
