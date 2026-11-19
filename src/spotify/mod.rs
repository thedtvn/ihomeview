//! Spotify Connect boundary between the dashboard and the experimental librespot backend.
//!
//! The UI only depends on [`SpotifyState`], [`Playback`], [`SpotifyCommand`], [`start`] and
//! [`send`]. The backend signs in by pairing (a code approved at spotify.com/pair), logs a
//! librespot session in, keeps Spotify's reusable stored credential, observes the account's
//! Spotify Connect devices without becoming a player, and remote-controls the active device.
//! See `README.md` in this directory.
use std::cell::{Cell, RefCell};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use librespot::oauth::DeviceAuthClientBuilder;
use nsh_core::{prelude::*, storage::Storage};
use tokio::sync::mpsc;

use crate::debug_log as dlog;

pub mod connect;
mod library;
mod library_io;
mod session;

/// Connection and playback state shown by the dashboard.
#[derive(Clone, Debug, PartialEq)]
pub enum SpotifyState {
    /// The backend is not available in this build, with a short reason for the user.
    Unavailable(String),
    /// No saved Spotify login; the user can sign in.
    SignedOut,
    /// Pairing: the user enters `code` at `url` (spotify.com/pair) on a phone or computer.
    AwaitingApproval { code: String, url: String },
    /// Signing in or connecting to Spotify.
    Connecting,
    /// Connected to the user's Spotify account.
    Connected(Playback),
    /// A recoverable failure, with a short message for the user.
    Error(String),
}

/// What the account's active Spotify Connect device is playing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playback {
    /// The active device, such as the Windows PC; `None` when nothing is active.
    pub device: Option<String>,
    pub title: Option<String>,
    pub artists: Option<String>,
    pub album: Option<String>,
    /// HTTPS artwork URL, when Spotify provides one.
    pub artwork_url: Option<String>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub duration_ms: u64,
    /// Whether play/pause/next/previous are currently accepted for the active device.
    pub can_control: bool,
    /// Shuffle is on for the current context (also true with Smart Shuffle).
    pub shuffle: bool,
    /// Smart Shuffle (shuffle + recommendations): `modes.context_enhancement = RECOMMENDATION`.
    pub smart_shuffle: bool,
    pub repeat: RepeatMode,
    /// The playing context (playlist/album) URI, when Spotify reports one.
    pub context_uri: Option<String>,
    /// Spotify URI of the loaded track; saving uses this snapshot, not a later track.
    pub track_uri: Option<String>,
    /// The active/last remote device volume, 0..=100; None when not reported.
    pub volume_percent: Option<u8>,
    pub can_set_volume: bool,
}

/// Repeat state of the active device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    /// Repeat the playlist/album.
    Context,
    /// Repeat the current track.
    Track,
}

impl RepeatMode {
    /// Spotify's order when tapping repeat: off -> context -> track -> off.
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::Context,
            RepeatMode::Context => RepeatMode::Track,
            RepeatMode::Track => RepeatMode::Off,
        }
    }
}

/// One of the user's playlists (or Liked Songs) for quick play.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistSummary {
    /// `spotify:playlist:...` or `spotify:user:<name>:collection` (Liked Songs).
    pub uri: String,
    pub name: String,
    /// HTTPS cover image, when the playlist has one.
    pub image_url: Option<String>,
    /// True only for editable playlists (and Liked Songs), never Spotify DJ.
    pub can_add_tracks: bool,
    pub song_count: Option<u32>,
}

pub const DJ_PLAYLIST_ID: &str = "37i9dQZF1EYkqdzj48dyYq";

impl PlaylistSummary {
    pub fn is_dj(&self) -> bool {
        self.uri == format!("spotify:playlist:{DJ_PLAYLIST_ID}")
    }

    pub fn is_liked_songs(&self) -> bool {
        self.uri.ends_with(":collection")
    }
}

/// State of the add-to-playlist sheet. Requests refer to a snapshot of the track URI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SaveTrackState {
    pub track_uri: String,
    pub loading: bool,
    pub busy: bool,
    /// Playlist/collection URIs already containing the track (do not add duplicates).
    pub saved_in: Vec<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// User actions sent from the dashboard to the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotifyCommand {
    /// Start pairing (shows a code to approve at spotify.com/pair).
    SignIn,
    SignOut,
    PlayPause,
    Next,
    Previous,
    /// Spotify's shuffle button: off -> shuffle -> Smart Shuffle -> off.
    ToggleShuffle,
    /// Advance repeat: off -> context -> track -> off.
    CycleRepeat,
    /// Jump to this position in the current track, in milliseconds.
    SeekTo(u64),
    /// Volume of the remote playback device, NOT this iPad, 0..=100.
    SetVolume(u8),
}

const KEY_USERNAME: &str = "spotify.username";
/// Spotify's reusable "stored credential" (base64). Never the password.
const KEY_CREDENTIALS: &str = "spotify.stored_credentials";
const KEY_DEVICE: &str = "spotify.observer_device_id";
/// Spotify for Desktop ("keymaster"): enabled for device pairing, and the client the macOS
/// identity of the fantasybot-red librespot fork logs in as.
pub(crate) const PAIR_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub(crate) const PAIR_SCOPES: &[&str] = &["streaming"];
/// The OAuth refresh token from pairing: a second way back in when the stored credential fails.
const KEY_REFRESH: &str = "spotify.oauth_refresh_token";
/// Left by an old backend; removed on start.
const KEY_OLD_REFRESH: &str = "spotify.refresh_token";
/// Reconnect delays after an unexpected disconnect; the last one repeats.
const RECONNECT_DELAYS_S: &[u64] = &[2, 5, 10, 30, 60];

thread_local! {
    /// Bumped by each sign-in or cancel, so a superseded pairing stops quietly.
    static PAIRING: Cell<u64> = const { Cell::new(0) };
    /// Bumped by sign-in / sign-out, so a pending reconnect stops quietly.
    static LOGIN_GEN: Cell<u64> = const { Cell::new(0) };
    /// Unexpected disconnects in a row (reset once connected).
    static RECONNECTS: Cell<usize> = const { Cell::new(0) };
    /// The running session's request channel (engine thread only).
    static SESSION: RefCell<Option<mpsc::UnboundedSender<session::Request>>> = const { RefCell::new(None) };
}

fn observer_device_id() -> String {
    if let Some(id) = Storage::get_item(KEY_DEVICE).filter(|id| id.len() == 40) {
        return id;
    }
    let id = connect::new_device_id();
    let _ = Storage::set_item(KEY_DEVICE, id.clone());
    id
}

fn stop_session() {
    SESSION.with(|s| {
        if let Some(tx) = s.borrow_mut().take() {
            let _ = tx.send(session::Request::Stop);
        }
    });
}

fn forget_login() {
    Storage::remove_item(KEY_USERNAME);
    Storage::remove_item(KEY_CREDENTIALS);
    Storage::remove_item(KEY_REFRESH);
}

fn saved_refresh() -> Option<String> {
    Storage::get_item(KEY_REFRESH).filter(|t| !t.is_empty())
}

/// The best saved way to log in: the stored credential, else the refresh token. After a stored
/// credential failure (`prefer_refresh`), the refresh token goes first.
fn saved_login_method(prefer_refresh: bool) -> Option<session::Login> {
    let stored = saved_login().map(|(username, auth_data)| session::Login::Stored { username, auth_data });
    let refresh = saved_refresh().map(session::Login::Refresh);
    if prefer_refresh { refresh.or(stored) } else { stored.or(refresh) }
}

/// Reconnects after an unexpected disconnect, with backoff, unless the user signed in/out since.
fn schedule_reconnect(state: Signal<SpotifyState>, prefer_refresh: bool) {
    let attempt = RECONNECTS.with(|r| {
        let n = r.get();
        r.set(n + 1);
        n
    });
    let delay = RECONNECT_DELAYS_S[attempt.min(RECONNECT_DELAYS_S.len() - 1)];
    let generation = LOGIN_GEN.with(Cell::get);
    dlog::warn(format!("spotify: reconnecting in {delay}s (attempt {})", attempt + 1));
    Owner::detached().run(move || {
        spawn(async move {
            timer(std::time::Duration::from_secs(delay)).await;
            if LOGIN_GEN.with(Cell::get) != generation || SESSION.with(|s| s.borrow().is_some()) {
                return;
            }
            match saved_login_method(prefer_refresh || attempt % 2 == 1) {
                Some(login) => start_session_keeping(state, login, true),
                None => state.set(SpotifyState::SignedOut),
            }
        });
    });
}

fn saved_login() -> Option<(String, Vec<u8>)> {
    let username = Storage::get_item(KEY_USERNAME).filter(|u| !u.is_empty())?;
    let data = Storage::get_item(KEY_CREDENTIALS).and_then(|c| BASE64.decode(c).ok())?;
    (!data.is_empty()).then_some((username, data))
}

/// Starts a session and pumps its events into `state`.
///
/// The librespot work runs on NSH's tokio runtime. `background` (a `tokio::spawn`) needs the
/// runtime's context, which press handlers do not have, so it is called from inside an engine
/// task (tasks are polled in the runtime's context).
fn start_session(state: Signal<SpotifyState>, login: session::Login) {
    start_session_keeping(state, login, false);
}

/// `keep_screen`: a reconnect keeps showing the last playback instead of "Connecting".
fn start_session_keeping(state: Signal<SpotifyState>, login: session::Login, keep_screen: bool) {
    stop_session();
    if !(keep_screen && matches!(state.get_untracked(), SpotifyState::Connected(_))) {
        state.set(SpotifyState::Connecting);
    }
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    SESSION.with(|s| *s.borrow_mut() = Some(request_tx.clone()));
    let device_id = observer_device_id();

    // Detached: the sign-in form that calls this is disposed as soon as the state becomes
    // `Connecting`, and a task owned by it would be cancelled before it ever ran.
    Owner::detached().run(move || spawn(async move {
        dlog::info("spotify: session task started");
        let _worker = background(session::run(login, device_id, event_tx, request_rx));
        while let Some(event) = event_rx.recv().await {
            // Ignore events from a session that has since been replaced or stopped.
            let current = SESSION.with(|s| s.borrow().as_ref().is_some_and(|tx| tx.same_channel(&request_tx)));
            if !current {
                break;
            }
            match event {
                session::Event::Credentials { username, auth_data } => {
                    dlog::info(format!("spotify: saving stored credential for '{username}'"));
                    let _ = Storage::set_item(KEY_USERNAME, username);
                    let _ = Storage::set_item(KEY_CREDENTIALS, BASE64.encode(auth_data));
                }
                session::Event::SaveState { state: result, mutation } => {
                    // A late membership read cannot overwrite an in-progress save or the status
                    // of a different track's sheet.
                    let current = save_state().get_untracked();
                    if current.track_uri == result.track_uri && (!current.busy || mutation) {
                        save_state().set(result);
                    }
                }
                session::Event::Playlists(list) => playlists().set(list),
                session::Event::RefreshToken(token) => {
                    dlog::info("spotify: saving rotated refresh token");
                    let _ = Storage::set_item(KEY_REFRESH, token);
                }
                session::Event::Playback(playback) => {
                    if !matches!(state.get_untracked(), SpotifyState::Connected(_)) {
                        dlog::info("spotify: connected");
                    }
                    RECONNECTS.with(|r| r.set(0));
                    state.set(SpotifyState::Connected(playback));
                }
                session::Event::Error(message) => {
                    dlog::warn(format!("spotify: {message}"));
                    // Keep showing playback; surface command errors without dropping the session.
                    if !matches!(state.get_untracked(), SpotifyState::Connected(_)) {
                        state.set(SpotifyState::Error(message));
                    }
                }
                session::Event::Ended { message, signed_out } => {
                    SESSION.with(|s| *s.borrow_mut() = None);
                    let pending = save_state().get_untracked();
                    if pending.busy || pending.loading {
                        save_state().update(|s| {
                            s.loading = false;
                            s.busy = false;
                            s.error = Some(if pending.busy {
                                "Connection lost during save. Check Spotify before retrying.".into()
                            } else {
                                "Connection lost while checking saved songs. Reopen this menu to retry.".into()
                            });
                        });
                    }
                    if signed_out {
                        // That way in is dead; keep the other one (stored credential vs refresh).
                        Storage::remove_item(KEY_CREDENTIALS);
                    }
                    let Some(message) = message else {
                        state.set(SpotifyState::SignedOut);
                        break;
                    };
                    if saved_login_method(signed_out).is_some() {
                        // Drops, kicks and expiries are retried quietly; the user never re-pairs
                        // while a saved credential or refresh token still works.
                        schedule_reconnect(state, signed_out);
                    } else {
                        forget_login();
                        state.set(SpotifyState::Error(message));
                    }
                    break;
                }
            }
        }
    }));
}

/// Sign-in by pairing (OAuth device authorization, RFC 8628): Spotify issues a short code, the
/// iPad shows it, and the user approves it at spotify.com/pair on a phone or computer while the
/// app polls. No browser on the iPad and nothing listening on a port.
fn sign_in(state: Signal<SpotifyState>) {
    LOGIN_GEN.with(|g| g.set(g.get() + 1));
    dlog::info("sign-in: requesting a pairing code (device authorization)");
    stop_session();
    state.set(SpotifyState::Connecting);
    let attempt = PAIRING.with(|p| {
        let next = p.get() + 1;
        p.set(next);
        next
    });

    // Detached engine task: outlives the button, and has the tokio context `background` needs.
    Owner::detached().run(move || {
        spawn(async move {
            dlog::info("sign-in: pairing task started");
            let client = match DeviceAuthClientBuilder::new(PAIR_CLIENT_ID, PAIR_SCOPES.to_vec()).build() {
                Ok(c) => std::sync::Arc::new(c),
                Err(e) => {
                    dlog::error(format!("pairing client: {e:?}"));
                    state.set(SpotifyState::Error(format!("Spotify sign-in unavailable ({e}).")));
                    return;
                }
            };
            let requester = client.clone();
            let auth = match background(async move { requester.request_device_code_async().await }).await {
                Ok(Ok(auth)) => auth,
                Ok(Err(e)) => {
                    dlog::error(format!("pairing code request failed: {e:?}"));
                    state.set(SpotifyState::Error(format!("Could not get a Spotify pairing code ({e}).")));
                    return;
                }
                Err(_) => return,
            };
            dlog::info(format!("sign-in: pairing code {} at {}", auth.user_code(), auth.url()));
            if PAIRING.with(|p| p.get()) != attempt {
                return;
            }
            state.set(SpotifyState::AwaitingApproval {
                code: auth.user_code().to_owned(),
                url: auth.verification_uri().trim_start_matches("https://").to_owned(),
            });
            let poller = client.clone();
            let token = background(async move { poller.poll_for_token_async(&auth).await }).await;
            if PAIRING.with(|p| p.get()) != attempt {
                dlog::info("sign-in: pairing cancelled");
                return;
            }
            match token {
                Ok(Ok(token)) => {
                    dlog::info(format!("sign-in: paired, access token received (refresh token: {})", !token.refresh_token.is_empty()));
                    if !token.refresh_token.is_empty() {
                        let _ = Storage::set_item(KEY_REFRESH, token.refresh_token.clone());
                    }
                    RECONNECTS.with(|r| r.set(0));
                    start_session(state, session::Login::Token(token.access_token));
                }
                Ok(Err(e)) => {
                    dlog::error(format!("pairing failed: {e:?}"));
                    let text = e.to_string();
                    state.set(if text.contains("access_denied") {
                        SpotifyState::SignedOut
                    } else if text.contains("expired") {
                        SpotifyState::Error("The pairing code expired. Try again.".into())
                    } else {
                        SpotifyState::Error(format!("Spotify pairing failed ({text})."))
                    });
                }
                Err(_) => {}
            }
        });
    });
}

/// Starts the backend once for the dashboard, restoring a saved login when one exists.
pub fn start(state: Signal<SpotifyState>) {
    Storage::remove_item(KEY_OLD_REFRESH);
    match saved_login_method(false) {
        Some(login) => {
            dlog::info(format!("spotify: restoring saved login {login:?}"));
            start_session(state, login);
        }
        None => state.set(SpotifyState::SignedOut),
    }
}

/// Sends a user action to the backend.
pub fn send(state: Signal<SpotifyState>, command: SpotifyCommand) {
    match command {
        SpotifyCommand::SignIn => sign_in(state),
        SpotifyCommand::SignOut => {
            PAIRING.with(|p| p.set(p.get() + 1));
            LOGIN_GEN.with(|g| g.set(g.get() + 1));
            dlog::info("spotify: sign out");
            stop_session();
            forget_login();
            playlists().set(Vec::new());
            save_state().set(SaveTrackState::default());
            state.set(SpotifyState::SignedOut);
        }
        _ => {
            let playback = match state.get_untracked() {
                SpotifyState::Connected(p) => p,
                _ => Playback::default(),
            };
            let remote = match command {
                SpotifyCommand::Next => connect::RemoteCommand::SkipNext,
                SpotifyCommand::Previous => connect::RemoteCommand::SkipPrev,
                SpotifyCommand::PlayPause if playback.is_playing => connect::RemoteCommand::Pause,
                SpotifyCommand::PlayPause => connect::RemoteCommand::Resume,
                SpotifyCommand::ToggleShuffle => connect::RemoteCommand::SetShuffle(next_shuffle(&playback)),
                SpotifyCommand::CycleRepeat => connect::RemoteCommand::SetRepeat(playback.repeat.next()),
                SpotifyCommand::SetVolume(percent) => {
                    send_request(session::Request::SetVolume(percent.min(100)));
                    return;
                }
                SpotifyCommand::SeekTo(ms) => connect::RemoteCommand::SeekTo(ms),
                SpotifyCommand::SignIn | SpotifyCommand::SignOut => return,
            };
            send_request(session::Request::Command(remote));
        }
    }
}

/// The shuffle state after a tap, like Spotify's own button.
pub fn next_shuffle(playback: &Playback) -> connect::ShuffleMode {
    use connect::ShuffleMode::*;
    match (playback.shuffle, playback.smart_shuffle) {
        (_, true) => Off,
        (true, false) => Smart,
        (false, false) => On,
    }
}

fn send_request(request: session::Request) {
    SESSION.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.send(request);
        }
    });
}

thread_local! {
    /// The user's playlists for quick play (filled once connected; empty until then).
    static PLAYLISTS: RefCell<Option<Signal<Vec<PlaylistSummary>>>> = const { RefCell::new(None) };
    static SAVE_STATE: RefCell<Option<Signal<SaveTrackState>>> = const { RefCell::new(None) };
}

/// The user's playlists (Liked Songs first), loaded after connecting. Call from a component;
/// the same signal is returned every time.
pub fn playlists() -> Signal<Vec<PlaylistSummary>> {
    PLAYLISTS.with(|p| {
        *p.borrow_mut().get_or_insert_with(|| Owner::detached().run(|| signal(Vec::new())))
    })
}

/// Starts playing `uri` (a playlist or Liked Songs) on the active device.
pub fn play_context(uri: &str) {
    dlog::info(format!("spotify: play context {uri}"));
    send_request(session::Request::Command(connect::RemoteCommand::PlayContext(uri.to_owned())));
}

/// Shared status for the current add-to-playlist sheet.
pub fn save_state() -> Signal<SaveTrackState> {
    SAVE_STATE.with(|s| {
        *s.borrow_mut().get_or_insert_with(|| Owner::detached().run(|| signal(SaveTrackState::default())))
    })
}

fn connected_request(request: session::Request) -> bool {
    SESSION.with(|s| s.borrow().as_ref().is_some_and(|tx| tx.send(request).is_ok()))
}

/// Loads membership for a snapshot of the current track. No changes to the user's library.
pub fn inspect_saved_track(track_uri: &str) {
    let state = save_state();
    state.set(SaveTrackState { track_uri: track_uri.to_owned(), loading: true, ..Default::default() });
    if !connected_request(session::Request::InspectSaved(track_uri.to_owned())) {
        state.update(|s| { s.loading = false; s.error = Some("Connect to Spotify first.".into()); });
    }
}

/// Adds the snapshot track to the chosen destinations, never DJ and never a read-only playlist.
/// Existing membership is left alone. Only the backend's confirmed response sets success.
pub fn save_track_to(track_uri: &str, destinations: Vec<String>) {
    let status = save_state();
    if status.get_untracked().busy { return; }
    let allowed = playlists().get_untracked();
    let mut targets = Vec::new();
    for uri in destinations {
        if !allowed.iter().any(|p| p.uri == uri && p.can_add_tracks && !p.is_dj()) {
            status.update(|s| s.error = Some("That playlist cannot be edited.".into()));
            return;
        }
        if !targets.contains(&uri) { targets.push(uri); }
    }
    if !library::track_uri_is_valid(track_uri) || targets.is_empty() {
        status.update(|s| s.error = Some("Choose a track and at least one playlist.".into()));
        return;
    }
    status.update(|s| { s.busy = true; s.error = None; s.notice = None; });
    if !connected_request(session::Request::SaveTrack { track_uri: track_uri.to_owned(), destinations: targets }) {
        status.update(|s| { s.busy = false; s.error = Some("Connect to Spotify first.".into()); });
    }
}
