//! Spotify Connect observer: watches the account's devices and remote-controls the active one.
//!
//! This uses Spotify's undocumented Connect protocol through librespot 0.8.0:
//!
//! * The app registers a **hidden, non-playing** device (`can_be_player = false`, `hidden =
//!   true`, `is_active = false`) with `PUT /connect-state/v1/devices/{id}`; the reply is the
//!   current `Cluster` (devices + player state).
//! * Updates arrive on the Dealer websocket at `hm://connect-state/v1/cluster`.
//! * Commands go to the active device with
//!   `POST /connect-state/v1/player/command/from/{our id}/to/{target id}`.
//!
//! It never transfers playback and never marks itself active. Everything here is unverified
//! against live Spotify until tested with a real account (see `README.md`).
use std::time::{SystemTime, UNIX_EPOCH};

use librespot_protocol::{
    connect::{
        Capabilities, Cluster, Device, DeviceInfo, MemberType, PutStateReason, PutStateRequest,
    },
    devices::DeviceType,
    player::PlayerState,
};
use protobuf::{EnumOrUnknown, MessageField};

use super::Playback;

pub const OBSERVER_NAME: &str = "ihomeview";

/// A fresh random device id for this iPad's observer (persisted by the caller).
pub fn new_device_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The registration that makes this app a hidden, controller-only Connect member.
pub fn observer_request(device_id: &str, client_id: &str, session_id: &str) -> PutStateRequest {
    let capabilities = Capabilities {
        can_be_player: false,
        hidden: true,
        is_observable: true,
        is_controllable: false,
        gaia_eq_connect_id: true,
        needs_full_player_state: true,
        supports_command_request: true,
        supports_transfer_command: false,
        supports_gzip_pushes: true,
        disable_volume: true,
        command_acks: true,
        ..Default::default()
    };
    let info = DeviceInfo {
        can_play: false,
        name: OBSERVER_NAME.into(),
        device_id: device_id.into(),
        device_type: EnumOrUnknown::new(DeviceType::TABLET),
        device_software_version: librespot_core::version::SEMVER.into(),
        spirc_version: librespot_core::version::SPOTIFY_SPIRC_VERSION.into(),
        client_id: client_id.into(),
        capabilities: MessageField::some(capabilities),
        ..Default::default()
    };
    PutStateRequest {
        member_type: EnumOrUnknown::new(MemberType::CONNECT_STATE),
        put_state_reason: EnumOrUnknown::new(PutStateReason::NEW_DEVICE),
        is_active: false,
        device: MessageField::some(Device {
            device_info: MessageField::some(info),
            player_state: MessageField::some(PlayerState {
                session_id: session_id.into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Track details resolved from Spotify metadata when the cluster lacks them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artists: Option<String>,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// `spotify:image:<hex>` or a bare hex file id → an HTTPS artwork URL; URLs pass through.
pub fn image_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("https://") {
        return Some(value.to_owned());
    }
    let id = value.strip_prefix("spotify:image:").unwrap_or(value);
    (id.len() == 40 && id.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("https://i.scdn.co/image/{}", id.to_ascii_lowercase()))
}

/// Details carried in the cluster's track metadata map (keys used by Spotify clients).
pub fn track_info_from_metadata(meta: &std::collections::HashMap<String, String>) -> TrackInfo {
    let artwork = ["image_large_url", "image_url", "image_xlarge_url", "image_small_url"]
        .iter()
        .find_map(|k| meta.get(*k).and_then(|v| image_url(v)));
    TrackInfo {
        title: non_empty(meta.get("title")),
        artists: non_empty(meta.get("artist_name")),
        album: non_empty(meta.get("album_title")),
        artwork_url: artwork,
    }
}

/// The current track's URI, when something is loaded.
pub fn track_uri(cluster: &Cluster) -> Option<String> {
    cluster
        .player_state
        .as_ref()
        .and_then(|p| p.track.as_ref())
        .map(|t| t.uri.clone())
        .filter(|u| !u.is_empty())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Maps the cluster to what the dashboard shows. `extra` fills fields the cluster lacks.
pub fn playback_from_cluster(
    cluster: &Cluster,
    own_device_id: &str,
    extra: Option<&TrackInfo>,
    now_ms: i64,
) -> Playback {
    let active = cluster.active_device_id.as_str();
    if active.is_empty() || active == own_device_id {
        return Playback::default();
    }
    let device = cluster
        .device
        .get(active)
        .map(|d| d.name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| Some("Spotify device".into()));
    let Some(state) = cluster.player_state.as_ref() else {
        return Playback { device, ..Default::default() };
    };
    let mut info = state
        .track
        .as_ref()
        .map(|t| track_info_from_metadata(&t.metadata))
        .unwrap_or_default();
    if let Some(extra) = extra {
        info.title = info.title.or_else(|| extra.title.clone());
        info.artists = info.artists.or_else(|| extra.artists.clone());
        info.album = info.album.or_else(|| extra.album.clone());
        info.artwork_url = info.artwork_url.or_else(|| extra.artwork_url.clone());
    }
    let is_playing = state.is_playing && !state.is_paused;
    let duration_ms = state.duration.max(0) as u64;
    let mut progress = state.position_as_of_timestamp.max(0);
    if is_playing && state.timestamp > 0 {
        let speed = if state.playback_speed > 0.0 { state.playback_speed } else { 1.0 };
        progress += ((now_ms - state.timestamp).max(0) as f64 * speed) as i64;
    }
    let mut progress_ms = progress.max(0) as u64;
    if duration_ms > 0 {
        progress_ms = progress_ms.min(duration_ms);
    }
    Playback {
        device,
        title: info.title,
        artists: info.artists,
        album: info.album,
        artwork_url: info.artwork_url,
        is_playing,
        progress_ms,
        duration_ms,
        can_control: track_uri(cluster).is_some(),
    }
}

/// Commands this app sends to the active device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCommand {
    Pause,
    Resume,
    SkipNext,
    SkipPrev,
}

impl RemoteCommand {
    /// The endpoint name; matches the serde tags in librespot-core `dealer/protocol/request.rs`.
    pub fn endpoint(self) -> &'static str {
        match self {
            RemoteCommand::Pause => "pause",
            RemoteCommand::Resume => "resume",
            RemoteCommand::SkipNext => "skip_next",
            RemoteCommand::SkipPrev => "skip_prev",
        }
    }

    pub fn body(self) -> String {
        serde_json::json!({ "command": { "endpoint": self.endpoint() } }).to_string()
    }
}

pub fn command_path(from: &str, to: &str) -> String {
    format!("/connect-state/v1/player/command/from/{from}/to/{to}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::player::ProvidedTrack;
    use protobuf::Message;

    fn cluster(active: &str, playing: bool) -> Cluster {
        let mut c = Cluster::new();
        c.active_device_id = active.into();
        c.device.insert(
            "pc".into(),
            DeviceInfo { name: "DESKTOP-PC".into(), ..Default::default() },
        );
        let mut track = ProvidedTrack::new();
        track.uri = "spotify:track:4uLU6hMCjMI75M1A2tKUQC".into();
        track.metadata.insert("title".into(), "Song".into());
        track.metadata.insert("artist_name".into(), "Artist".into());
        track.metadata.insert("album_title".into(), "Album".into());
        track.metadata.insert(
            "image_large_url".into(),
            "spotify:image:ab67616d0000b273AABBCCDDEEFF0011223344ff".into(),
        );
        let mut state = PlayerState::new();
        state.track = MessageField::some(track);
        state.is_playing = playing;
        state.is_paused = !playing;
        state.duration = 200_000;
        state.position_as_of_timestamp = 10_000;
        state.timestamp = 1_000_000;
        c.player_state = MessageField::some(state);
        c
    }

    #[test]
    fn maps_active_pc_playback() {
        let p = playback_from_cluster(&cluster("pc", true), "me", None, 1_005_000);
        assert_eq!(p.device.as_deref(), Some("DESKTOP-PC"));
        assert_eq!(p.title.as_deref(), Some("Song"));
        assert_eq!(p.artists.as_deref(), Some("Artist"));
        assert_eq!(p.album.as_deref(), Some("Album"));
        assert_eq!(
            p.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b273aabbccddeeff0011223344ff")
        );
        assert!(p.is_playing && p.can_control);
        assert_eq!(p.progress_ms, 15_000);
        assert_eq!(p.duration_ms, 200_000);
    }

    #[test]
    fn paused_progress_does_not_advance() {
        let p = playback_from_cluster(&cluster("pc", false), "me", None, 9_000_000);
        assert!(!p.is_playing);
        assert_eq!(p.progress_ms, 10_000);
    }

    #[test]
    fn no_active_device_or_self_is_nothing_playing() {
        assert_eq!(playback_from_cluster(&cluster("", true), "me", None, 0), Playback::default());
        assert_eq!(playback_from_cluster(&cluster("me", true), "me", None, 0), Playback::default());
    }

    #[test]
    fn metadata_fallback_fills_missing_fields() {
        let mut c = cluster("pc", true);
        c.player_state.as_mut().unwrap().track.as_mut().unwrap().metadata.clear();
        let extra = TrackInfo { title: Some("From API".into()), ..Default::default() };
        let p = playback_from_cluster(&c, "me", Some(&extra), 0);
        assert_eq!(p.title.as_deref(), Some("From API"));
        assert!(p.artwork_url.is_none());
    }

    #[test]
    fn image_urls() {
        assert_eq!(image_url("https://i.scdn.co/image/x").as_deref(), Some("https://i.scdn.co/image/x"));
        assert!(image_url("spotify:image:short").is_none());
        assert!(image_url("").is_none());
    }

    #[test]
    fn observer_never_claims_playback() {
        let r = observer_request("dev1", "cid", "sess");
        assert!(!r.is_active);
        let info = r.device.device_info.as_ref().unwrap();
        let caps = info.capabilities.as_ref().unwrap();
        assert!(!info.can_play && !caps.can_be_player && caps.hidden);
        assert_eq!(info.device_id, "dev1");
        assert_eq!(r.put_state_reason.enum_value(), Ok(PutStateReason::NEW_DEVICE));
        assert!(!r.write_to_bytes().unwrap().is_empty());
    }

    #[test]
    fn command_bodies_and_paths() {
        assert_eq!(RemoteCommand::Pause.body(), r#"{"command":{"endpoint":"pause"}}"#);
        assert_eq!(RemoteCommand::SkipPrev.endpoint(), "skip_prev");
        assert_eq!(
            command_path("me", "pc"),
            "/connect-state/v1/player/command/from/me/to/pc"
        );
        assert_eq!(new_device_id().len(), 40);
    }
}
