//! Offering: an action that puts a fake image into mikro.
//!
//! Run:  cargo run -p arkitekt-examples --bin upload_demo
//!
//! The app connects to the Arkitekt server in `$FAKTS_URL` (or the public
//! deployment at go.arkitekt.live), authenticates once in your browser, and
//! then `upload_fake_image` is callable from the UI, from a workflow, or from
//! any other app. Each call returns the new dataset, which travels as a
//! `@mikro/arraydataset` reference, so callers can keep working with it.

use arkitekt::{action, run, App, Task};
use mikro::{axes_for, ArrayDataset, Mikro};
use ndarray::ArrayD;

/// Upload Fake Image
///
/// Synthesises a noisy multi-channel z-stack with a few gaussian blobs per
/// channel and stores it in mikro as a new array dataset.
///
/// # Arguments
/// * `name` - What the dataset is called
/// * `channels` - How many channels to generate
/// * `depth` - How many z-planes to generate
/// * `size` - Edge length of each plane, in pixels
///
/// # Returns
/// The uploaded dataset
#[action]
async fn upload_fake_image(
    #[port(default = "Rusty blobs")] name: String,
    #[port(default = 2)] channels: i64,
    #[port(default = 5)] depth: i64,
    #[port(default = 256)] size: i64,
    #[inject] mikro: Mikro,
    task: Task,
) -> anyhow::Result<ArrayDataset> {
    anyhow::ensure!(
        channels > 0 && depth > 0 && size > 0,
        "all dimensions must be positive"
    );

    task.progress(10, "Synthesising");
    let image = fake_image(channels as usize, depth as usize, size as usize);

    task.progress(40, "Uploading");
    // mikro wants its axes ordered time -> channel -> space.
    let dataset = mikro
        .create_array_dataset(&name, &image, axes_for(&["c", "z", "y", "x"]))
        .await?;

    task.log(format!(
        "uploaded {} with shape {:?}",
        dataset.id, dataset.shape
    ));
    Ok(dataset)
}

/// Gaussian blobs, spread over the channels, on a floor of noise (16-bit).
fn fake_image(channels: usize, depth: usize, size: usize) -> ArrayD<u16> {
    let n_blobs = 2 * channels;
    let blobs: Vec<(usize, f64, f64, f64)> = (0..n_blobs)
        .map(|i| {
            let f = size as f64;
            (
                i % channels,
                f * (0.15 + 0.7 * rand::random::<f64>()),
                f * (0.15 + 0.7 * rand::random::<f64>()),
                f * (0.04 + 0.06 * rand::random::<f64>()),
            )
        })
        .collect();
    let mid = (depth as f64 - 1.0) / 2.0;

    ArrayD::from_shape_fn(vec![channels, depth, size, size], |ix| {
        let (c, z, y, x) = (ix[0], ix[1] as f64, ix[2] as f64, ix[3] as f64);
        let signal: f64 = blobs
            .iter()
            .filter(|(channel, ..)| *channel == c)
            .map(|(_, by, bx, sigma)| {
                let d2 = (y - by).powi(2) + (x - bx).powi(2) + (4.0 * (z - mid)).powi(2);
                40_000.0 * (-d2 / (2.0 * sigma * sigma)).exp()
            })
            .sum();
        let noise = 500.0 * rand::random::<f64>();
        (signal + noise).min(u16::MAX as f64) as u16
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Nothing here connects: an App is a declaration. `run` is what
    // authenticates, registers the action and blocks.
    let app = App::new("upload-demo", "0.1.0")
        .description("Puts fake images into mikro, from Rust")
        .service(mikro::service)
        .action(upload_fake_image);

    run(app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkitekt::rekuest::{Action, PortKind};

    #[test]
    fn declares_ports_like_python() {
        let definition = upload_fake_image.definition();
        assert_eq!(definition.name, "Upload Fake Image");
        let keys: Vec<_> = definition.args.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            ["name", "channels", "depth", "size"],
            "mikro and task are injected, not ports"
        );
        assert!(definition
            .args
            .iter()
            .all(|p| p.nullable && p.default.is_some()));
        assert_eq!(definition.returns[0].kind, PortKind::Structure);
        assert_eq!(
            definition.returns[0].identifier.as_deref(),
            Some("@mikro/arraydataset")
        );
    }

    #[test]
    fn fakes_an_image() {
        let image = fake_image(2, 3, 32);
        assert_eq!(image.shape(), [2, 3, 32, 32]);
        assert!(
            image.iter().any(|&v| v > 1000),
            "blobs rise above the noise"
        );
    }
}
