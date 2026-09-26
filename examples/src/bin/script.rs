//! Use Arkitekt services from a script, without serving any actions
//! (the Rust twin of Python's `with easy("script", mikro) as mikro:`).
//!
//! ```sh
//! cargo run -p arkitekt-examples --bin script
//! ```

use mikro::{axes_for, Mikro};
use ndarray::ArrayD;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = arkitekt::easy("rusty-script", "0.1.0")
        .service(mikro::service())
        .connect()
        .await?;
    let mikro = rt.require::<Mikro>()?;

    let data: ArrayD<f32> = ArrayD::from_shape_fn(vec![64, 64], |ix| (ix[0] * ix[1]) as f32);
    let image = mikro
        .create_array_dataset("gradient", &data, axes_for(&["y", "x"]))
        .await?;
    println!(
        "created {} ({}) with shape {:?}",
        image.name, image.id, image.shape
    );

    let back = mikro.read_array::<f32>(&image, 0).await?;
    assert_eq!(back, data);
    println!("read it back: {:?}", back.shape());
    Ok(())
}
