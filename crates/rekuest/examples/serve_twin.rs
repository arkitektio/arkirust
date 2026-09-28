//! The Rust twin of `tests/fixtures/serve/twin.py`, served over HTTP.
//!
//!     cargo run -p rekuest --features serve --example serve_twin -- 8766 /tmp/rust-twin.db

#[path = "../tests/twin/mod.rs"]
mod twin;

use rekuest::serve::{configure, AuthRequest, History, ServeOptions, Unauthorized};
use rekuest::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8766);
    let db = args.next().unwrap_or_else(|| "twin.db".into());

    let options = ServeOptions::default()
        .history(History::Sqlite(db.into()))
        .auth(|request| match request {
            AuthRequest::Http { headers, .. } => {
                match headers.get("authorization").and_then(|h| h.to_str().ok()) {
                    Some("Bearer good") => Ok("tester".into()),
                    _ => Err(Unauthorized("bad credentials".into())),
                }
            }
            AuthRequest::WebSocket(init) => match init.token.as_deref() {
                Some("good") => Ok("tester".into()),
                _ => Err(Unauthorized("bad credentials".into())),
            },
        });
    let (router, agent) = configure(
        axum::Router::new(),
        twin::registry(),
        Context::default(),
        options,
    )?;
    agent.start().await?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    agent.shutdown().await;
    Ok(())
}
