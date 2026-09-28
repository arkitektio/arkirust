//! Streaming video to a LiveKit server that is only on the mesh, through the
//! UDP tunnel: two mesh nodes (publisher, subscriber) each join LiveKit via a
//! local forward (signaling) and their TURN relay (media, relay-only ICE);
//! the subscriber must decode the publisher's frames.
//!
//! Needs the mesh lab with its LiveKit server:
//!
//! ```sh
//! testing/mesh-lab/lab.sh up && testing/mesh-lab/lab.sh livekit
//! eval "$(testing/mesh-lab/lab.sh env)"
//! cargo test -p arkitekt-lovekit --features livekit --test lab_livekit
//! ```

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use livekit::track::RemoteTrack;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit::{Room, RoomEvent};
use lovekit::{relay_options, VideoPublisher};
use mesh::driver::{Session, SessionOptions};

struct Env {
    url: String,
    key: String,
    host: String,
    api_key: String,
    api_secret: String,
}

fn env() -> Option<Env> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    Some(Env {
        url: var("ARKITEKT_TEST_MESH_URL")?,
        key: var("ARKITEKT_TEST_MESH_KEY")?,
        host: var("ARKITEKT_TEST_LIVEKIT_HOST")?,
        api_key: var("ARKITEKT_TEST_LIVEKIT_KEY")?,
        api_secret: var("ARKITEKT_TEST_LIVEKIT_SECRET")?,
    })
}

/// A LiveKit access token (HS256 JWT), as the lovekit server mints them.
fn token(env: &Env, identity: &str, room: &str) -> String {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let header = b64(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = b64(serde_json::json!({
        "iss": env.api_key, "sub": identity, "name": identity,
        "nbf": now - 10, "exp": now + 600,
        "video": {"room": room, "roomJoin": true, "canPublish": true, "canSubscribe": true},
    })
    .to_string()
    .as_bytes());
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(env.api_secret.as_bytes()).unwrap();
    mac.update(format!("{header}.{claims}").as_bytes());
    format!("{header}.{claims}.{}", b64(&mac.finalize().into_bytes()))
}

/// A mesh node, and a LiveKit room joined through it.
async fn join(
    env: &Env,
    dir: &std::path::Path,
    who: &str,
    room: &str,
) -> (
    Session,
    Room,
    tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
) {
    let mut options =
        SessionOptions::new(dir.join(who), format!("t-lk-{who}-{}", std::process::id()));
    options.control_url = Some(env.url.clone());
    options.auth_key = Some(env.key.clone());
    options.ephemeral = true;
    let mut session = Session::start(options).await.unwrap();
    let signaling = session.forward(&env.host, 7880).await.unwrap();
    let turn = session.turn().await.unwrap();
    let (room, events) = Room::connect(
        &format!("ws://{signaling}"),
        &token(env, who, room),
        relay_options(turn.urls, turn.username, turn.credential),
    )
    .await
    .unwrap_or_else(|e| panic!("{who} could not join the room: {e}"));
    (session, room, events)
}

#[tokio::test(flavor = "multi_thread")]
async fn video_streams_to_livekit_over_the_mesh_udp_tunnel() {
    let Some(env) = env() else {
        eprintln!("skipping: the mesh lab's LiveKit is not up (testing/mesh-lab/lab.sh livekit)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let room_name = format!("lab-room-{}", std::process::id());

    let (_pub_session, publisher, _) = join(&env, dir.path(), "publisher", &room_name).await;
    let (w, h) = (64u32, 48u32);
    let camera = VideoPublisher::publish(&publisher, "camera", w, h)
        .await
        .unwrap();
    let feeding = tokio::spawn(async move {
        let mut t = 0i64;
        loop {
            let shade = (t / 33_000 % 255) as u8;
            let frame: Vec<u8> = (0..w * h)
                .flat_map(|_| [shade, 128, 255 - shade, 255])
                .collect();
            camera.push_rgba(&frame, t);
            t += 33_000;
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    });

    let (_sub_session, _subscriber, mut events) =
        join(&env, dir.path(), "subscriber", &room_name).await;
    let received = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(event) = events.recv().await {
            if let RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(track),
                ..
            } = event
            {
                let mut frames = NativeVideoStream::new(track.rtc_track());
                // A few frames: the stream is flowing, not a single keyframe.
                let mut sizes = Vec::new();
                while sizes.len() < 5 {
                    let frame = frames.next().await.expect("the video stream ended");
                    sizes.push((frame.buffer.width(), frame.buffer.height()));
                }
                return sizes;
            }
        }
        panic!("the room closed before the video track arrived");
    })
    .await
    .expect("no video within 60 s");
    feeding.abort();
    assert!(
        received.iter().all(|&(fw, fh)| fw > 0 && fh > 0),
        "{received:?}"
    );
    assert_eq!(received.last().copied(), Some((w, h)), "{received:?}");
}
