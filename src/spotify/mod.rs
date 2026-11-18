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
}

const KEY_USERNAME: &str = "spotify.username";
/// Spotify's reusable "stored credential" (base64). Never the password.
const KEY_CREDENTIALS: &str = "spotify.stored_credentials";
const KEY_DEVICE: &str = "spotify.observer_device_id";
/// Spotify for Desktop ("keymaster"): enabled for device pairing, and the client the macOS
/// identity of the fantasybot-red librespot fork logs in as.
const PAIR_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const PAIR_SCOPES: &[&str] = &["streaming"];
/// Left by the old OAuth backend; removed on start.
const KEY_OLD_REFRESH: &str = "spotify.refresh_token";

thread_local! {
    /// Bumped by each sign-in or cancel, so a superseded pairing stops quietly.
    static PAIRING: Cell<u64> = const { Cell::new(0) };
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
    stop_session();
    state.set(SpotifyState::Connecting);
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
                session::Event::Playback(playback) => {
                    if !matches!(state.get_untracked(), SpotifyState::Connected(_)) {
                        dlog::info("spotify: connected");
                    }
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
                    if signed_out {
                        forget_login();
                    }
                    state.set(match message {
                        Some(m) => SpotifyState::Error(m),
                        None => SpotifyState::SignedOut,
                    });
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
                    dlog::info("sign-in: paired, access token received");
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
    match saved_login() {
        Some((username, auth_data)) => {
            dlog::info(format!("spotify: restoring saved login for '{username}'"));
            start_session(state, session::Login::Stored { username, auth_data });
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
            dlog::info("spotify: sign out");
            stop_session();
            forget_login();
            state.set(SpotifyState::SignedOut);
        }
        SpotifyCommand::PlayPause | SpotifyCommand::Next | SpotifyCommand::Previous => {
            let remote = match (command, state.get_untracked()) {
                (SpotifyCommand::Next, _) => connect::RemoteCommand::SkipNext,
                (SpotifyCommand::Previous, _) => connect::RemoteCommand::SkipPrev,
                (_, SpotifyState::Connected(p)) if p.is_playing => connect::RemoteCommand::Pause,
                _ => connect::RemoteCommand::Resume,
            };
            SESSION.with(|s| {
                if let Some(tx) = s.borrow().as_ref() {
                    let _ = tx.send(session::Request::Command(remote));
                }
            });
        }
    }
}
