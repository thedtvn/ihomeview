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

use librespot::protocol::{
    connect::{
        Capabilities, Cluster, Device, DeviceInfo, MemberType, PutStateReason, PutStateRequest,
    },
    devices::DeviceType,
    player::PlayerState,
};
use protobuf::{EnumOrUnknown, MessageField};

use super::{Playback, RepeatMode};

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
        device_software_version: librespot::core::version::SEMVER.into(),
        spirc_version: librespot::core::version::SPOTIFY_SPIRC_VERSION.into(),
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

/// Connect volume uses the full u16 range, not percentage.
pub fn volume_percent(raw: u32) -> u8 {
    ((raw.min(65_535) as u64 * 100 + 32_767) / 65_535) as u8
}

pub fn volume_raw(percent: u8) -> i32 {
    ((u32::from(percent.min(100)) * 65_535 + 50) / 100) as i32
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
    if active == own_device_id && !active.is_empty() {
        return Playback::default();
    }
    // After a pause, Spotify often drops the active device but keeps the last player state:
    // keep showing that track (paused) instead of "Nothing playing".
    let idle = active.is_empty();
    if idle && track_uri(cluster).is_none() {
        return Playback::default();
    }
    let device = if idle {
        None
    } else {
        cluster
            .device
            .get(active)
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| Some("Spotify device".into()))
    };
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
    let is_playing = !idle && state.is_playing && !state.is_paused;
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
    // Paused playback can have no active id; expose volume only if there is one unambiguous
    // remote player. The session fills in its last-active device when there are several.
    let remote = cluster.device.get(active).or_else(|| {
        let mut candidates = cluster.device.iter().filter(|(id, d)| id.as_str() != own_device_id && d.can_play);
        let (_, only) = candidates.next()?;
        candidates.next().is_none().then_some(only)
    });
    Playback {
        device,
        title: info.title,
        artists: info.artists,
        album: info.album,
        artwork_url: info.artwork_url,
        is_playing,
        progress_ms,
        duration_ms,
        // Idle (paused, no active device): still controllable; commands go to the last device.
        can_control: track_uri(cluster).is_some(),
        shuffle: state.options.as_ref().is_some_and(|o| o.shuffling_context),
        smart_shuffle: state
            .options
            .as_ref()
            .and_then(|o| o.modes.get("context_enhancement"))
            .is_some_and(|m| m == "RECOMMENDATION"),
        repeat: match state.options.as_ref() {
            Some(o) if o.repeating_track => RepeatMode::Track,
            Some(o) if o.repeating_context => RepeatMode::Context,
            _ => RepeatMode::Off,
        },
        context_uri: Some(state.context_uri.clone()).filter(|u| !u.is_empty()),
        track_uri: track_uri(cluster),
        volume_percent: remote.map(|d| volume_percent(d.volume)),
        can_set_volume: remote.is_some_and(|d| {
            !d.capabilities.as_ref().is_some_and(|c| c.disable_volume)
        }),
    }
}

/// Shuffle states of Spotify's shuffle button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShuffleMode {
    Off,
    On,
    /// Smart Shuffle: shuffle plus recommended tracks.
    Smart,
}

/// Commands this app sends to the active device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteCommand {
    Pause,
    Resume,
    SkipNext,
    SkipPrev,
    SetShuffle(ShuffleMode),
    SetRepeat(RepeatMode),
    /// Position in milliseconds.
    SeekTo(u64),
    /// Play a playlist / album / Liked Songs context from its start.
    PlayContext(String),
}

impl RemoteCommand {
    /// The endpoint name; matches the serde tags in librespot-core `dealer/protocol/request.rs`.
    pub fn endpoint(&self) -> &'static str {
        match self {
            RemoteCommand::Pause => "pause",
            RemoteCommand::Resume => "resume",
            RemoteCommand::SkipNext => "skip_next",
            RemoteCommand::SkipPrev => "skip_prev",
            RemoteCommand::SetShuffle(_) => "set_options",
            RemoteCommand::SetRepeat(_) => "set_options",
            RemoteCommand::SeekTo(_) => "seek_to",
            RemoteCommand::PlayContext(_) => "play",
        }
    }

    /// The JSON body of `POST /connect-state/v1/player/command/...`. Field names follow the
    /// commands librespot's connect receives (`dealer/protocol/request.rs`).
    pub fn body(&self) -> String {
        let logging = serde_json::json!({});
        let command = match self {
            // Exactly what Spotify's web player sends (captured 2026-11-19).
            RemoteCommand::SetShuffle(mode) => serde_json::json!({
                "endpoint": self.endpoint(),
                "shuffling_context": *mode != ShuffleMode::Off,
                "modes": { "context_enhancement": if *mode == ShuffleMode::Smart { "RECOMMENDATION" } else { "NONE" } },
                "logging_params": logging,
            }),
            RemoteCommand::SetRepeat(mode) => serde_json::json!({
                "endpoint": self.endpoint(),
                "repeating_context": *mode != RepeatMode::Off,
                "repeating_track": *mode == RepeatMode::Track,
                "logging_params": logging,
            }),
            RemoteCommand::SeekTo(ms) => serde_json::json!({
                "endpoint": self.endpoint(), "value": ms, "position": 0, "logging_params": logging,
            }),
            RemoteCommand::PlayContext(uri) => serde_json::json!({
                "endpoint": self.endpoint(),
                "context": { "uri": uri, "url": format!("context://{uri}"), "metadata": {} },
                "play_origin": { "feature_identifier": "ihomeview", "feature_version": env!("CARGO_PKG_VERSION") },
                "options": { "license": "premium", "skip_to": {}, "player_options_override": {} },
                "logging_params": logging,
            }),
            _ => serde_json::json!({ "endpoint": self.endpoint() }),
        };
        serde_json::json!({ "command": command }).to_string()
    }
}

pub fn command_path(from: &str, to: &str) -> String {
    format!("/connect-state/v1/player/command/from/{from}/to/{to}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot::protocol::player::ProvidedTrack;
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
    fn self_active_is_nothing_playing() {
        assert_eq!(playback_from_cluster(&cluster("me", true), "me", None, 0), Playback::default());
    }

    /// Seen live: after pausing on the PC, `active_device_id` is empty but the last player state
    /// (paused track, context, shuffle) is still in the cluster.
    #[test]
    fn no_active_device_keeps_last_track_paused() {
        let p = playback_from_cluster(&cluster("", true), "me", None, 1_005_000);
        assert_eq!(p.title.as_deref(), Some("Song"));
        assert!(p.device.is_none());
        assert!(!p.is_playing && p.can_control);
        assert_eq!(p.progress_ms, 10_000);
        let mut empty = cluster("", true);
        empty.player_state.as_mut().unwrap().track = MessageField::none();
        assert_eq!(playback_from_cluster(&empty, "me", None, 0), Playback::default());
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
    fn remote_volume_scale_and_capability() {
        assert_eq!(volume_percent(0), 0);
        assert_eq!(volume_percent(65_535), 100);
        assert_eq!(volume_raw(100), 65_535);
        assert_eq!(volume_raw(255), 65_535);
        assert_eq!(volume_percent(volume_raw(50) as u32), 50);
        let mut c = cluster("pc", false);
        let device = c.device.get_mut("pc").unwrap();
        device.can_play = true;
        device.volume = 32_768;
        let p = playback_from_cluster(&c, "me", None, 0);
        assert_eq!(p.volume_percent, Some(50));
        assert!(p.can_set_volume);
        assert_eq!(p.track_uri.as_deref(), Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC"));
        c.active_device_id.clear();
        assert!(playback_from_cluster(&c, "me", None, 0).can_set_volume);
        c.device.get_mut("pc").unwrap().capabilities = MessageField::some(Capabilities {
            disable_volume: true, ..Default::default()
        });
        assert!(!playback_from_cluster(&c, "me", None, 0).can_set_volume);
    }

    #[test]
    fn new_command_bodies() {
        let v = |c: RemoteCommand| serde_json::from_str::<serde_json::Value>(&c.body()).unwrap()["command"].clone();
        let shuffle = v(RemoteCommand::SetShuffle(ShuffleMode::On));
        assert_eq!(shuffle["endpoint"], "set_options");
        assert_eq!(shuffle["shuffling_context"], true);
        assert_eq!(shuffle["modes"]["context_enhancement"], "NONE");
        let smart = v(RemoteCommand::SetShuffle(ShuffleMode::Smart));
        assert_eq!(smart["shuffling_context"], true);
        assert_eq!(smart["modes"]["context_enhancement"], "RECOMMENDATION");
        let off = v(RemoteCommand::SetShuffle(ShuffleMode::Off));
        assert_eq!(off["shuffling_context"], false);
        let repeat = v(RemoteCommand::SetRepeat(RepeatMode::Track));
        assert_eq!(repeat["endpoint"], "set_options");
        assert_eq!(repeat["repeating_context"], true);
        assert_eq!(repeat["repeating_track"], true);
        let off = v(RemoteCommand::SetRepeat(RepeatMode::Off));
        assert_eq!(off["repeating_context"], false);
        let seek = v(RemoteCommand::SeekTo(61_500));
        assert_eq!(seek["endpoint"], "seek_to");
        assert_eq!(seek["value"], 61_500);
        let play = v(RemoteCommand::PlayContext("spotify:playlist:abc".into()));
        assert_eq!(play["endpoint"], "play");
        assert_eq!(play["context"]["uri"], "spotify:playlist:abc");
    }

    #[test]
    fn maps_shuffle_repeat_and_context() {
        let mut c = cluster("pc", true);
        let state = c.player_state.as_mut().unwrap();
        state.context_uri = "spotify:playlist:xyz".into();
        let mut opts = librespot::protocol::player::ContextPlayerOptions::new();
        opts.shuffling_context = true;
        opts.repeating_context = true;
        opts.repeating_track = true;
        opts.modes.insert("context_enhancement".into(), "RECOMMENDATION".into());
        state.options = MessageField::some(opts);
        let p = playback_from_cluster(&c, "me", None, 1_000_000);
        assert!(p.shuffle && p.smart_shuffle);
        assert_eq!(p.repeat, RepeatMode::Track);
        assert_eq!(p.context_uri.as_deref(), Some("spotify:playlist:xyz"));
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
