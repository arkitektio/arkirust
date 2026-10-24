//! The embassy control on the host: `cargo run` exits 0 when the scenario passed.

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    match embassy_control::run(seed).await {
        Ok(server) => {
            server.check();
            println!("embassy control: the scripted scenario passed");
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("embassy control: the session ended: {error}");
            std::process::exit(1);
        }
    }
}
