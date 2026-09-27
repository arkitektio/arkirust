//! The Rust twin of `fixtures/serve/twin.py`.

#![allow(dead_code)]

use futures::stream::{self, Stream};
use rekuest::{action, Registry, State, StateMut, Task};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, State)]
#[state(name = "CameraState", locks = ["camera"])]
pub struct CameraState {
    pub connected: bool,
    pub exposure_ms: f64,
    pub tags: Vec<String>,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            connected: false,
            exposure_ms: 10.0,
            tags: vec![],
        }
    }
}

/// Set Exposure
///
/// Changes the exposure time.
#[action]
pub async fn set_exposure(exposure_ms: f64, camera: StateMut<CameraState>) -> anyhow::Result<f64> {
    camera.update(|c| c.exposure_ms = exposure_ms)?;
    Ok(exposure_ms)
}

/// Add Tag
#[action]
pub async fn add_tag(tag: String, camera: StateMut<CameraState>) -> anyhow::Result<i64> {
    Ok(camera.update(|c| {
        c.tags.push(tag);
        c.tags.len() as i64
    })?)
}

/// Count Up
#[action]
pub async fn count_up(until: i64) -> impl Stream<Item = i64> {
    stream::iter(0..until)
}

/// Explode
#[action]
pub async fn explode() -> anyhow::Result<i64> {
    anyhow::bail!("boom")
}

/// Pausable
#[action]
pub async fn pausable(task: Task) -> String {
    task.pausepoint().await;
    "done".to_owned()
}

/// The twin registry. No getter actions: state is observable without them.
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .state(CameraState::default())
        .startup(|startup| async move {
            startup.set_state(CameraState {
                connected: true,
                ..CameraState::default()
            })?;
            Ok(())
        })
        .register(set_exposure)
        .register(add_tag)
        .register(count_up)
        .register(explode)
        .register(pausable);
    registry
}
