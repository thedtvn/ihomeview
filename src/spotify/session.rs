//! The live librespot session: login, observer registration, cluster updates and commands.
//!
//! Runs entirely on NSH's tokio runtime (via `background`), reporting [`Event`]s over a channel
//! that the engine-side task in `mod.rs` turns into `SpotifyState` changes.
use std::time::Duration;

use futures_util::StreamExt;
use librespot_core::{
    Session, SessionConfig, SpotifyUri, authentication::Credentials, dealer::protocol::Message,
};
use librespot_protocol::{
    connect::{Cluster, ClusterUpdate},
    metadata::Track,
};
use protobuf::Message as _;
use reqwest::Method;
use tokio::sync::mpsc;

use crate::debug_log as dlog;

use super::{
    Playback, auth,
    connect::{self, RemoteCommand, TrackInfo},
};

/// What the session reports to the UI side.
#[derive(Debug)]
pub enum Event {
    /// A (possibly rotated) refresh token to persist.
    RefreshToken(String),
    Playback(Playback),
    /// A recoverable error with a short user-facing message.
    Error(String),
    /// The session ended (signed out, expired or lost); `message` explains why if relevant.
    Ended { message: Option<String>, signed_out: bool },
}

/// Requests from the UI side to a running session.
#[derive(Debug)]
pub enum Request {
    Command(RemoteCommand),
    Stop,
}

/// How long each connection step may take before the session gives up with an error.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Holds the latest cluster plus resolved track metadata.
struct Observed {
    cluster: Option<Cluster>,
    extra: Option<(String, TrackInfo)>,
}

impl Observed {
    fn playback(&self, own_id: &str) -> Playback {
        match &self.cluster {
            Some(cluster) => {
                let extra = match (&self.extra, connect::track_uri(cluster)) {
                    (Some((uri, info)), Some(current)) if *uri == current => Some(info),
                    _ => None,
                };
                connect::playback_from_cluster(cluster, own_id, extra, connect::now_ms())
            }
            None => Playback::default(),
        }
    }
}

fn short(err: impl std::fmt::Display) -> String {
    let text = err.to_string();
    let text = text.lines().next().unwrap_or("").trim();
    let mut out: String = text.chars().take(120).collect();
    if out.is_empty() {
        out = "unknown error".into();
    }
    out
}

/// Resolves title/artist/album/artwork through Spotify's metadata service.
async fn resolve_track(session: &Session, uri: &str) -> Option<TrackInfo> {
    let parsed = SpotifyUri::from_uri(uri).ok()?;
    let bytes = session.spclient().get_track_metadata(&parsed).await.ok()?;
    let track = Track::parse_from_bytes(&bytes).ok()?;
    let artists: Vec<_> = track.artist.iter().filter_map(|a| a.name.clone()).collect();
    let album = track.album.as_ref();
    let artwork = album.and_then(|a| {
        let mut covers: Vec<_> = a
            .cover_group
            .as_ref()
            .map(|g| g.image.clone())
            .unwrap_or_default();
        covers.extend(a.cover.iter().cloned());
        covers.sort_by_key(|i| std::cmp::Reverse(i.width.unwrap_or(0)));
        covers
            .into_iter()
            .find_map(|i| i.file_id.map(|id| format!("https://i.scdn.co/image/{}", hex::encode(id))))
    });
    Some(TrackInfo {
        title: track.name.clone(),
        artists: (!artists.is_empty()).then(|| artists.join(", ")),
        album: album.and_then(|a| a.name.clone()),
        artwork_url: artwork,
    })
}

async fn send_command(session: &Session, own_id: &str, target: &str, cmd: RemoteCommand) -> Result<(), String> {
    session
        .spclient()
        .request_as_json(
            &Method::POST,
            &connect::command_path(own_id, target),
            None,
            Some(&cmd.body()),
        )
        .await
        .map(|_| ())
        .map_err(short)
}

/// Runs one session until it ends. `device_id` is the persisted observer id.
pub async fn run(
    refresh_token: String,
    device_id: String,
    events: mpsc::UnboundedSender<Event>,
    mut requests: mpsc::UnboundedReceiver<Request>,
) {
    let ended = |message: Option<String>, signed_out: bool| {
        if let Some(m) = &message {
            dlog::error(format!("session ended: {m}"));
        } else {
            dlog::info("session ended");
        }
        Event::Ended { message, signed_out }
    };

    // 1. Fresh access token (refresh tokens can rotate).
    dlog::info(format!("step 1/4: refreshing access token (device {device_id})"));
    let tokens = match auth::refresh(&refresh_token).await {
        Ok(t) => {
            dlog::info(format!(
                "step 1/4 ok: access token received, expires in {}s, rotated refresh: {}",
                t.expires_in,
                t.refresh_token.as_deref().is_some_and(|r| !r.is_empty() && r != refresh_token)
            ));
            t
        }
        Err(auth::AuthError::Exchange(e)) if e.starts_with("HTTP 400") || e.starts_with("HTTP 401") => {
            let _ = events.send(ended(Some("Spotify sign-in expired. Please sign in again.".into()), true));
            return;
        }
        Err(e) => {
            let _ = events.send(ended(Some(e.to_string()), false));
            return;
        }
    };
    if let Some(rotated) = tokens.refresh_token.filter(|t| !t.is_empty() && *t != refresh_token) {
        let _ = events.send(Event::RefreshToken(rotated));
    }

    // 2. Session with the same client id as the token, and our persisted observer id.
    let mut config = SessionConfig::default();
    config.client_id = auth::CLIENT_ID.into();
    config.device_id = device_id.clone();
    let session = Session::new(config, None);
    dlog::info(format!("session created, client id {}", session.client_id()));

    // Listeners must be registered before the dealer starts.
    let dealer = session.dealer();
    let connection_ids = dealer.listen_for("hm://pusher/v1/connections/", |m: Message| {
        m.headers
            .get("Spotify-Connection-Id")
            .cloned()
            .ok_or_else(|| librespot_core::Error::failed_precondition("missing connection id"))
    });
    let cluster_updates =
        dealer.listen_for("hm://connect-state/v1/cluster", Message::from_raw::<ClusterUpdate>);
    let (mut connection_ids, mut cluster_updates) = match (connection_ids, cluster_updates) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            let _ = events.send(ended(Some(format!("Spotify setup failed ({}).", short(e))), false));
            return;
        }
    };

    dlog::info("step 2/4: connecting to Spotify access point (apresolve + login)");
    match tokio::time::timeout(
        STEP_TIMEOUT,
        session.connect(Credentials::with_access_token(tokens.access_token), false),
    )
    .await
    {
        Ok(Ok(())) => dlog::info(format!(
            "step 2/4 ok: logged in as {} (country {})",
            session.username(),
            session.country()
        )),
        Ok(Err(e)) => {
            dlog::error(format!("session.connect error: {e:?}"));
            let _ = events.send(ended(Some(format!("Spotify login failed ({}).", short(e))), false));
            return;
        }
        Err(_) => {
            let _ = events.send(ended(
                Some(format!("Spotify login timed out after {}s.", STEP_TIMEOUT.as_secs())),
                false,
            ));
            session.shutdown();
            return;
        }
    }
    dlog::info("step 3/4: starting dealer websocket");
    match tokio::time::timeout(STEP_TIMEOUT, session.dealer().start()).await {
        Ok(Ok(())) => dlog::info("step 3/4 ok: dealer connected"),
        Ok(Err(e)) => {
            dlog::error(format!("dealer start error: {e:?}"));
            let _ = events.send(ended(Some(format!("Spotify connection failed ({}).", short(e))), false));
            session.shutdown();
            return;
        }
        Err(_) => {
            let _ = events.send(ended(
                Some(format!("Spotify dealer timed out after {}s.", STEP_TIMEOUT.as_secs())),
                false,
            ));
            session.shutdown();
            return;
        }
    }
    dlog::info("step 4/4: waiting for connection id to register hidden observer");

    let mut observed = Observed { cluster: None, extra: None };
    let mut registered = false;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let _ = events.send(Event::Playback(Playback::default()));

    loop {
        tokio::select! {
            id = connection_ids.next() => {
                let id = match id {
                    Some(Ok(id)) => id,
                    Some(Err(e)) => {
                        dlog::error(format!("connection id error: {e:?}"));
                        let _ = events.send(ended(Some("Spotify connection lost.".into()), false));
                        break;
                    }
                    None => {
                        let _ = events.send(ended(Some("Spotify connection lost.".into()), false));
                        break;
                    }
                };
                dlog::info("got connection id; registering observer device");
                session.set_connection_id(&id);
                let request = connect::observer_request(&device_id, &session.client_id(), &session.session_id());
                match session.spclient().put_connect_state_request(&request).await {
                    Ok(bytes) => {
                        registered = true;
                        match Cluster::parse_from_bytes(&bytes) {
                            Ok(cluster) => {
                                dlog::info(format!(
                                    "observer registered; {} devices, active '{}'",
                                    cluster.device.len(),
                                    cluster.active_device_id
                                ));
                                observed.cluster = Some(cluster);
                            }
                            Err(e) => dlog::warn(format!("observer registered; cluster parse failed: {e}")),
                        }
                    }
                    Err(e) => {
                        dlog::error(format!("put_connect_state_request error: {e:?}"));
                        let _ = events.send(Event::Error(format!("Could not watch Spotify devices ({}).", short(e))));
                    }
                }
            }
            update = cluster_updates.next() => {
                let Some(update) = update else {
                    let _ = events.send(ended(Some("Spotify connection lost.".into()), false));
                    break;
                };
                match update {
                    Ok(mut update) => {
                        if let Some(cluster) = update.cluster.take() {
                            if observed.cluster.as_ref().map(|c| &c.active_device_id) != Some(&cluster.active_device_id) {
                                dlog::info(format!("cluster: active device now '{}'", cluster.active_device_id));
                            }
                            observed.cluster = Some(cluster);
                        }
                    }
                    Err(e) => dlog::warn(format!("cluster update parse error: {e}")),
                }
            }
            request = requests.recv() => {
                match request {
                    None | Some(Request::Stop) => break,
                    Some(Request::Command(cmd)) => {
                        dlog::info(format!("command {cmd:?}"));
                        let target = observed.cluster.as_ref().map(|c| c.active_device_id.clone()).unwrap_or_default();
                        if !registered || target.is_empty() || target == device_id {
                            let _ = events.send(Event::Error("No active Spotify device to control.".into()));
                        } else if let Err(e) = send_command(&session, &device_id, &target, cmd).await {
                            dlog::error(format!("command failed: {e}"));
                            let _ = events.send(Event::Error(format!("Command failed ({e}).")));
                        } else {
                            dlog::info(format!("command {cmd:?} sent to {target}"));
                        }
                    }
                }
            }
            _ = tick.tick() => {
                if session.is_invalid() {
                    let _ = events.send(ended(Some("Spotify session ended.".into()), false));
                    break;
                }
            }
        }

        // Resolve missing metadata for the current track once per track.
        if let Some(uri) = observed.cluster.as_ref().and_then(connect::track_uri) {
            let needs = observed.extra.as_ref().is_none_or(|(u, _)| *u != uri);
            let pb = observed.playback(&device_id);
            if needs && (pb.title.is_none() || pb.artwork_url.is_none()) {
                let info = resolve_track(&session, &uri).await.unwrap_or_default();
                observed.extra = Some((uri, info));
            }
        }
        let _ = events.send(Event::Playback(observed.playback(&device_id)));
    }

    if registered {
        let _ = session.spclient().put_connect_state_inactive(false).await;
    }
    session.dealer().close().await;
    session.shutdown();
}
