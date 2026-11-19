//! The live librespot session: login, observer registration, cluster updates and commands.
//!
//! Runs entirely on NSH's tokio runtime (via `background`), reporting [`Event`]s over a channel
//! that the engine-side task in `mod.rs` turns into `SpotifyState` changes.
use std::time::Duration;

use futures_util::StreamExt;
use librespot::core::{
    Session, SessionConfig, SpotifyUri, authentication::Credentials, dealer::protocol::Message,
};
use librespot::protocol::{
    authentication::AuthenticationType,
    connect::{Cluster, ClusterUpdate},
    metadata::Track,
};
use protobuf::Message as _;
use reqwest::Method;
use tokio::sync::mpsc;

use crate::debug_log as dlog;

use super::{
    Playback, PlaylistSummary,
    connect::{self, RemoteCommand, TrackInfo},
};

/// How the session logs in.
pub enum Login {
    /// First sign-in: an OAuth access token from Spotify's login page (keymaster client).
    Token(String),
    /// Later launches: Spotify's reusable stored credential from an earlier login.
    Stored { username: String, auth_data: Vec<u8> },
    /// Fallback: the OAuth refresh token from pairing, exchanged for a fresh access token.
    Refresh(String),
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Login::Token(_) => f.write_str("OAuthToken(<hidden>)"),
            Login::Stored { username, auth_data } => write!(f, "Stored({username}, {} bytes)", auth_data.len()),
            Login::Refresh(_) => f.write_str("RefreshToken(<hidden>)"),
        }
    }
}

/// What the session reports to the UI side.
#[derive(Debug)]
pub enum Event {
    /// Spotify's reusable credential to persist (replaces the password).
    Credentials { username: String, auth_data: Vec<u8> },
    /// A (possibly rotated) OAuth refresh token to persist.
    RefreshToken(String),
    /// The user's playlists for quick play (Liked Songs first).
    Playlists(Vec<PlaylistSummary>),
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

async fn send_command(session: &Session, own_id: &str, target: &str, cmd: &RemoteCommand) -> Result<(), String> {
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

/// A short, user-facing message for a login failure, and whether the saved login is unusable.
fn login_failure(err: &librespot::core::Error) -> (String, bool) {
    let text = format!("{err}");
    if text.contains("INVALID_CREDENTIALS") || text.contains("BadCredentials") || text.contains("UNKNOWN_IDENTIFIER") {
        ("Wrong Spotify email/username or password.".into(), true)
    } else if text.contains("Code challenge") {
        ("Spotify asked for an email/SMS code, which this app cannot do yet.".into(), false)
    } else if text.contains("TOO_MANY_ATTEMPTS") || text.contains("TRY_AGAIN_LATER") {
        ("Spotify says too many attempts. Try again later.".into(), false)
    } else if text.contains("PremiumAccountRequired") {
        ("Spotify Premium is required.".into(), false)
    } else {
        (format!("Spotify login failed ({}).", short(err)), false)
    }
}

/// The user's playlists from the rootlist (`/playlist/v2/user/{me}/rootlist`), Liked Songs first.
async fn load_playlists(session: &Session) -> Result<Vec<PlaylistSummary>, String> {
    use librespot::protocol::playlist4_external::SelectedListContent;
    let bytes = session.spclient().get_rootlist(0, Some(120)).await.map_err(short)?;
    let root = SelectedListContent::parse_from_bytes(&bytes).map_err(short)?;
    let mut out = vec![PlaylistSummary {
        uri: format!("spotify:user:{}:collection", session.username()),
        name: "Liked Songs".into(),
        image_url: None,
    }];
    let contents = root.contents.as_ref();
    let items = contents.map(|c| c.items.as_slice()).unwrap_or_default();
    let metas = contents.map(|c| c.meta_items.as_slice()).unwrap_or_default();
    for (i, item) in items.iter().enumerate() {
        let uri = item.uri.clone().unwrap_or_default();
        if !uri.starts_with("spotify:playlist:") {
            continue; // folders are "spotify:start-group/end-group"
        }
        let attrs = metas.get(i).and_then(|m| m.attributes.as_ref());
        let name = attrs.and_then(|a| a.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Playlist".into());
        let image_url = attrs.and_then(|a| {
            a.picture_size
                .iter()
                .find(|p| p.target_name.as_deref() == Some("default"))
                .or_else(|| a.picture_size.first())
                .and_then(|p| p.url.clone())
                .filter(|u| u.starts_with("https://"))
                .or_else(|| a.picture.as_ref().filter(|p| p.len() == 20).map(|p| format!("https://i.scdn.co/image/{}", hex::encode(p))))
        });
        out.push(PlaylistSummary { uri, name, image_url });
    }
    Ok(out)
}

/// Runs one session until it ends. `device_id` is the persisted observer id.
pub async fn run(
    login: Login,
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

    // Session with librespot's default client id. On iOS the fantasybot-red fork identifies as
    // macOS, so this is the desktop (keymaster) client the OAuth token was issued to.
    let mut config = SessionConfig::default();
    config.device_id = device_id.clone();
    let session = Session::new(config, None);
    dlog::info(format!(
        "session created: os {}, client id {}, login {login:?}",
        librespot::core::config::OS,
        session.client_id()
    ));

    // 1. Credentials for the access point.
    let credentials = match login {
        Login::Token(token) => {
            dlog::info("step 1/4: using OAuth access token from Spotify's login page");
            Credentials::with_access_token(token)
        }
        Login::Refresh(refresh) => {
            dlog::info("step 1/4: refreshing access token with the saved refresh token");
            let client = match librespot::oauth::DeviceAuthClientBuilder::new(super::PAIR_CLIENT_ID, super::PAIR_SCOPES.to_vec()).build() {
                Ok(c) => c,
                Err(e) => {
                    let _ = events.send(ended(Some(format!("Spotify sign-in unavailable ({e}).")), false));
                    return;
                }
            };
            match tokio::time::timeout(STEP_TIMEOUT, client.refresh_token_async(&refresh)).await {
                Ok(Ok(token)) => {
                    dlog::info("step 1/4 ok: access token refreshed");
                    if !token.refresh_token.is_empty() && token.refresh_token != refresh {
                        let _ = events.send(Event::RefreshToken(token.refresh_token.clone()));
                    }
                    Credentials::with_access_token(token.access_token)
                }
                Ok(Err(e)) => {
                    dlog::error(format!("refresh token rejected: {e:?}"));
                    let text = e.to_string();
                    let bad = text.contains("invalid_grant") || text.contains("invalid_client");
                    let _ = events.send(ended(Some(format!("Spotify sign-in expired ({}).", short(&e))), bad));
                    return;
                }
                Err(_) => {
                    let _ = events.send(ended(Some("Refreshing the Spotify login timed out.".into()), false));
                    return;
                }
            }
        }
        Login::Stored { username, auth_data } => {
            dlog::info("step 1/4: using saved stored credential");
            Credentials {
                username: Some(username),
                auth_type: AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
                auth_data,
            }
        }
    };

    // Listeners must be registered before the dealer starts.
    let dealer = session.dealer();
    let connection_ids = dealer.listen_for("hm://pusher/v1/connections/", |m: Message| {
        m.headers
            .get("Spotify-Connection-Id")
            .cloned()
            .ok_or_else(|| librespot::core::Error::failed_precondition("missing connection id"))
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
    match tokio::time::timeout(STEP_TIMEOUT, session.connect(credentials, false)).await {
        Ok(Ok(())) => {
            dlog::info(format!(
                "step 2/4 ok: logged in as {} (country {})",
                session.username(),
                session.country()
            ));
            // The access point hands back a reusable credential: keep it instead of a password.
            let auth_data = session.auth_data();
            if !auth_data.is_empty() {
                let _ = events.send(Event::Credentials { username: session.username(), auth_data });
            }
        }
        Ok(Err(e)) => {
            dlog::error(format!("session.connect error: {e:?}"));
            let (message, bad_login) = login_failure(&e);
            let _ = events.send(ended(Some(message), bad_login));
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
    match load_playlists(&session).await {
        Ok(list) => {
            dlog::info(format!("playlists: {} loaded", list.len()));
            let _ = events.send(Event::Playlists(list));
        }
        Err(e) => dlog::warn(format!("playlists: could not load ({e})")),
    }

    let mut observed = Observed { cluster: None, extra: None };
    let mut registered = false;
    // The last device that was active (commands go there while nothing is active).
    let mut last_active: Option<String> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_sent: Option<Playback> = None;
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
                                if !cluster.active_device_id.is_empty() {
                                    last_active = Some(cluster.active_device_id.clone());
                                } else {
                                    // Nothing active at start: the only other player device is the target.
                                    last_active = cluster
                                        .device
                                        .iter()
                                        .find(|(id, d)| *id != &device_id && d.can_play)
                                        .map(|(id, _)| id.clone());
                                }
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
                            if !cluster.active_device_id.is_empty() { last_active = Some(cluster.active_device_id.clone()); }
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
                        // After a pause Spotify clears the active device; the device that was
                        // playing still accepts resume/seek/play (verified live), so fall back to it.
                        let target = observed
                            .cluster
                            .as_ref()
                            .map(|c| c.active_device_id.clone())
                            .filter(|t| !t.is_empty())
                            .or_else(|| last_active.clone())
                            .unwrap_or_default();
                        if !registered || target.is_empty() || target == device_id {
                            let _ = events.send(Event::Error("No active Spotify device to control.".into()));
                        } else if let Err(e) = send_command(&session, &device_id, &target, &cmd).await {
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
        // Only wake the UI when something it shows changed. While playing, progress moves every
        // second; while paused nothing changes, so the card is not re-rendered (battery).
        let pb = observed.playback(&device_id);
        if last_sent.as_ref() != Some(&pb) {
            let _ = events.send(Event::Playback(pb.clone()));
            last_sent = Some(pb);
        }
    }

    if registered {
        let _ = session.spclient().put_connect_state_inactive(false).await;
    }
    session.dealer().close().await;
    session.shutdown();
}
