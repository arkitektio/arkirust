//! A minimal wasm driver for [`rekuest_session::Session`].
//!
//! The socket is a pair of channels to a [`FakeServer`] running as its own
//! task on the JS event loop; timers are `setTimeout`. A real one would put
//! a `web_sys::WebSocket` where the channels are: the session does not change.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::future::{select, Either};
use futures::StreamExt;
use gloo_timers::future::TimeoutFuture;
use rekuest_session::testing::{Echo, FakeServer, ServerOutput};
use rekuest_session::{to_text, ConnectionPolicy, Event, Session, SessionConfig, SessionError};

/// One connection: frames to the server, frames from it. The server ends
/// the connection by dropping its sender.
struct Socket {
    to_server: UnboundedSender<String>,
    from_server: UnboundedReceiver<String>,
}

/// Open a connection to the server task.
fn connect(server: &Rc<RefCell<FakeServer>>) -> Socket {
    let (to_server, mut inbound) = unbounded::<String>();
    let (outbound, from_server) = unbounded::<String>();
    server.borrow_mut().connected();
    let server = server.clone();
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(text) = inbound.next().await {
            server.borrow_mut().receive(&text);
            loop {
                let output = server.borrow_mut().poll();
                match output {
                    Some(ServerOutput::Frame(frame)) => {
                        let _ = outbound.unbounded_send(frame);
                    }
                    // Dropping `outbound` ends the agent's stream.
                    Some(ServerOutput::Drop) => return,
                    None => break,
                }
            }
        }
    });
    Socket {
        to_server,
        from_server,
    }
}

fn now(start: f64) -> Duration {
    Duration::from_secs_f64(((js_sys::Date::now() - start) / 1000.0).max(0.0))
}

/// Run the scripted scenario; the server, to [`FakeServer::check`].
pub async fn run() -> Result<Rc<RefCell<FakeServer>>, SessionError> {
    let start = js_sys::Date::now();
    let server = Rc::new(RefCell::new(FakeServer::scripted()));
    let seed = (js_sys::Math::random() * u64::MAX as f64) as u64;
    let mut session = Session::new(SessionConfig {
        policy: ConnectionPolicy {
            initial: Duration::from_millis(10),
            ..Default::default()
        },
        ..SessionConfig::new("s", seed)
    });
    let mut app = Echo::new("s");
    let mut socket: Option<Socket> = None;

    loop {
        session.handle_timeout(now(start));
        while let Some(event) = session.poll_event() {
            match event {
                Event::Connect => {
                    socket = Some(connect(&server));
                    let declaration =
                        serde_json::from_value(serde_json::json!({"name": "wasm-control"}))
                            .expect("a declaration");
                    session.connected(now(start), "token".into(), declaration);
                }
                Event::Close => socket = None,
                Event::Ended(error) => return Err(error),
                other => app.handle(&mut session, &other),
            }
        }
        while let Some(envelope) = session.poll_transmit() {
            if let Some(socket) = &socket {
                let _ = socket.to_server.unbounded_send(to_text(&envelope));
            }
        }
        if server.borrow().done() {
            return Ok(server);
        }

        let wait = session
            .poll_timeout()
            .map(|at| at.saturating_sub(now(start)))
            .unwrap_or(Duration::from_secs(3600));
        let timer = TimeoutFuture::new(wait.as_millis().min(u32::MAX as u128) as u32);
        let Some(open) = socket.as_mut() else {
            timer.await;
            continue;
        };
        match select(open.from_server.next(), timer).await {
            Either::Left((Some(text), _)) => session.handle_text(now(start), &text),
            Either::Left((None, _)) => {
                socket = None;
                session.handle_disconnect(now(start), "dropped");
            }
            Either::Right(_) => {}
        }
    }
}
