//! Joining LiveKit rooms, also on a media server that is only on the mesh,
//! and publishing video into them.
//!
//! WebRTC media is UDP, so it cannot use the mesh's HTTP proxy the way
//! GraphQL does. For a mesh alias (feature `mesh`), the room instead gets:
//! - signaling through a local TCP forward to the server
//!   (`Fakts::mesh_forward`);
//! - the mesh node's TURN relay on 127.0.0.1 as its only ICE server, with a
//!   relay-only policy (`Fakts::mesh_turn`). Every media packet goes to the
//!   relay, which sends it as UDP over the mesh to the server's mesh address.
//!
//! The server must advertise that address on its UDP port (LiveKit
//! `rtc.node_ip`, or `rtc.interfaces` limited to the tailnet interface).

use livekit::options::TrackPublishOptions;
use livekit::track::{LocalTrack, LocalVideoTrack, TrackSource};
use livekit::webrtc::peer_connection_factory::{IceServer, IceTransportsType, RtcConfiguration};
use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use livekit::{Room, RoomEvent, RoomOptions};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{Lovekit, LovekitError, Result};

/// Where and how to connect a room.
#[derive(Debug, Clone)]
pub struct RoomTarget {
    /// The signaling url: the alias' `ws(s)://…`, or a local forward.
    pub url: String,
    pub options: RoomOptions,
}

/// Room options that send all media through a TURN relay (`urls`,
/// `username`, `credential`): its only ICE server, relay-only.
pub fn relay_options(urls: Vec<String>, username: String, credential: String) -> RoomOptions {
    let mut rtc_config = RtcConfiguration::default();
    rtc_config.ice_servers = vec![IceServer {
        urls,
        username,
        password: credential,
    }];
    rtc_config.ice_transport_type = IceTransportsType::Relay;
    let mut options = RoomOptions::default();
    options.rtc_config = rtc_config;
    options
}

impl Lovekit {
    /// The url and options to connect a room with: the media server as it
    /// is, or (for a mesh alias, feature `mesh`) through the mesh.
    pub async fn room_target(&self) -> Result<RoomTarget> {
        let alias = self.livekit.as_ref().ok_or(LovekitError::NoLivekit)?;
        if !alias.is_mesh() {
            return Ok(RoomTarget {
                url: alias.to_ws_path(""),
                options: RoomOptions::default(),
            });
        }
        self.mesh_target(alias).await
    }

    #[cfg(feature = "mesh")]
    async fn mesh_target(&self, alias: &arkitekt::Alias) -> Result<RoomTarget> {
        let fakts = self
            .fakts
            .as_ref()
            .ok_or_else(|| LovekitError::Mesh("this client was built without fakts".into()))?;
        let mesh = |e: arkitekt::fakts::FaktsError| LovekitError::Mesh(e.to_string());
        let local = fakts.mesh_forward(alias).await.map_err(mesh)?;
        let turn = fakts.mesh_turn().await.map_err(mesh)?;
        // The forward carries the bytes as they are: TLS to 127.0.0.1 would
        // not match the server's certificate, so mesh servers speak plain ws
        // (the mesh itself is encrypted).
        let path = alias.path.as_deref().unwrap_or_default().trim_matches('/');
        let url = if path.is_empty() {
            format!("ws://{local}")
        } else {
            format!("ws://{local}/{path}")
        };
        Ok(RoomTarget {
            url,
            options: relay_options(turn.urls, turn.username, turn.credential),
        })
    }

    #[cfg(not(feature = "mesh"))]
    async fn mesh_target(&self, _alias: &arkitekt::Alias) -> Result<RoomTarget> {
        Err(LovekitError::Mesh(
            "enable the lovekit crate's `mesh` feature (and run the mesh natively)".into(),
        ))
    }

    /// Connect a room with a token lovekit handed out (`ensure_stream`,
    /// `join_broadcast`), through the mesh if the media server is only there.
    pub async fn connect_room(&self, token: &str) -> Result<(Room, UnboundedReceiver<RoomEvent>)> {
        let target = self.room_target().await?;
        tracing::debug!("connecting a LiveKit room at {}", target.url);
        Ok(Room::connect(&target.url, token, target.options).await?)
    }
}

/// A published video track fed with raw frames.
pub struct VideoPublisher {
    source: NativeVideoSource,
    track: LocalVideoTrack,
    width: u32,
    height: u32,
}

impl VideoPublisher {
    /// Publish a `width`×`height` camera track called `name` into `room`.
    pub async fn publish(room: &Room, name: &str, width: u32, height: u32) -> Result<Self> {
        let source = NativeVideoSource::new(VideoResolution { width, height }, false);
        let track =
            LocalVideoTrack::create_video_track(name, RtcVideoSource::Native(source.clone()));
        let options = TrackPublishOptions {
            source: TrackSource::Camera,
            ..Default::default()
        };
        room.local_participant()
            .publish_track(LocalTrack::Video(track.clone()), options)
            .await?;
        Ok(Self {
            source,
            track,
            width,
            height,
        })
    }

    pub fn track(&self) -> &LocalVideoTrack {
        &self.track
    }

    /// Push one frame of tightly packed RGBA pixels (`width * height * 4`
    /// bytes), captured at `timestamp_us`.
    pub fn push_rgba(&self, rgba: &[u8], timestamp_us: i64) {
        assert_eq!(
            rgba.len(),
            (self.width * self.height * 4) as usize,
            "a frame is width * height * 4 bytes of RGBA"
        );
        let mut buffer = I420Buffer::new(self.width, self.height);
        rgba_to_i420(rgba, self.width, self.height, &mut buffer);
        let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
        frame.timestamp_us = timestamp_us;
        self.source.capture_frame(&frame);
    }
}

/// RGBA to I420 (BT.601, studio range); chroma averaged over 2×2 blocks.
fn rgba_to_i420(rgba: &[u8], width: u32, height: u32, out: &mut I420Buffer) {
    let (stride_y, stride_u, stride_v) = out.strides();
    let (y_plane, u_plane, v_plane) = out.data_mut();
    let (w, h) = (width as usize, height as usize);
    let pixel = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        (rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32)
    };
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = pixel(x, y);
            y_plane[y * stride_y as usize + x] =
                (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    for cy in 0..h.div_ceil(2) {
        for cx in 0..w.div_ceil(2) {
            let (mut r, mut g, mut b, mut n) = (0, 0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                if x < w && y < h {
                    let (pr, pg, pb) = pixel(x, y);
                    (r, g, b, n) = (r + pr, g + pg, b + pb, n + 1);
                }
            }
            let (r, g, b) = (r / n, g / n, b / n);
            u_plane[cy * stride_u as usize + cx] =
                (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            v_plane[cy * stride_v as usize + cx] =
                (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_options_use_only_the_relay() {
        let options = relay_options(
            vec!["turn:127.0.0.1:3478?transport=udp".into()],
            "u".into(),
            "c".into(),
        );
        assert_eq!(
            options.rtc_config.ice_transport_type,
            IceTransportsType::Relay
        );
        assert_eq!(options.rtc_config.ice_servers.len(), 1);
        assert_eq!(options.rtc_config.ice_servers[0].password, "c");
    }

    #[test]
    fn rgba_converts_to_i420() {
        let (w, h) = (4, 2);
        // Left half white, right half black.
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                if i % w < w / 2 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect();
        let mut buffer = I420Buffer::new(w as u32, h as u32);
        rgba_to_i420(&rgba, w as u32, h as u32, &mut buffer);
        let (y, u, v) = buffer.data();
        assert_eq!(y[0], 235, "white is Y 235");
        assert_eq!(y[3], 16, "black is Y 16");
        assert_eq!((u[0], v[0]), (128, 128), "grey has no chroma");
    }
}
