//! An app that works with images in mikro.
//!
//! ```sh
//! cargo run -p arkitekt-examples --bin create_image
//! ```

use arkitekt::{action, run, App, Task};
use mikro::{axes_for, ArrayDataset, Mikro};
use ndarray::ArrayD;

/// Create Rusty Image
///
/// Creates an image filled with random noise.
///
/// # Arguments
/// * `name` - The name of the new image
/// * `size` - Edge length in pixels
///
/// # Returns
/// The created image
#[action]
async fn create_rusty_image(
    name: String,
    #[port(default = 512)] size: i64,
    #[inject] mikro: Mikro,
    task: Task,
) -> anyhow::Result<ArrayDataset> {
    anyhow::ensure!(size > 0, "size must be positive");
    let size = size as usize;
    task.progress(10, "generating noise");
    let data: ArrayD<u16> = ArrayD::from_shape_fn(vec![1, size, size], |_| rand::random::<u16>());
    task.progress(50, "uploading");
    let image = mikro
        .create_array_dataset(&name, &data, axes_for(&["c", "y", "x"]))
        .await?;
    task.log(format!("created {}", image.id));
    Ok(image)
}

/// Invert Image
///
/// Inverts a 16-bit image.
///
/// # Arguments
/// * `image` - The image to invert
///
/// # Returns
/// The inverted image
#[action]
async fn invert_image(
    image: ArrayDataset,
    #[inject] mikro: Mikro,
    task: Task,
) -> anyhow::Result<ArrayDataset> {
    task.progress(10, "downloading");
    let data = mikro.read_array::<u16>(&image, 0).await?;
    let inverted = data.mapv(|v| u16::MAX - v);
    task.progress(60, "uploading");
    let axes = axes_for(
        &image
            .axis_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    Ok(mikro
        .create_array_dataset(&format!("{} (inverted)", image.name), &inverted, axes)
        .await?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = App::new("rusty-images", "0.1.0")
        .description("Creates and processes images from Rust")
        .service(mikro::service())
        .action(create_rusty_image)
        .action(invert_image);

    run(app).await
}
