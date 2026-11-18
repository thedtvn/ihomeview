//! Spotify Connect boundary between the dashboard and the experimental librespot backend.
//!
//! The UI only depends on [`SpotifyState`], [`Playback`], [`SpotifyCommand`], [`start`] and
//! [`send`]. The backend signs in on Spotify's own login page (OAuth PKCE, `auth`), logs a
//! librespot session in, keeps Spotify's reusable stored credential, observes the account's
//! Spotify Connect devices without becoming a player, and remote-controls the active device.
//! See `README.md` in this directory.
use std::{cell::RefCell, rc::Rc};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use nsh_core::{
    auth_session::{AuthSessionOutcome, AuthSessionRequest},
    prelude::*,
    storage::Storage,
};
use tokio::sync::mpsc;

use crate::debug_log as dlog;

pub mod auth;
pub mod connect;
mod session;

/// Connection and playback state shown by the dashboard.
#[derive(Clone, Debug, PartialEq)]
pub enum SpotifyState {
    /// The backend is not available in this build, with a short reason for the user.
    Unavailable(String),
    /// No saved Spotify login; the user can sign in.
    SignedOut,
    /// Unused by the login5 backend; kept so older UI code still compiles.
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
    /// Open Spotify's login page.
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
const CALLBACK_SCHEME: &str = "ihomeview";
/// Left by the old OAuth backend; removed on start.
const KEY_OLD_REFRESH: &str = "spotify.refresh_token";

thread_local! {
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

/// Sign-in on Spotify's page: a loopback listener on 127.0.0.1:8898 catches the redirect while
/// the page shows in an in-app auth session; the code is exchanged for an access token.
fn sign_in(state: Signal<SpotifyState>) {
    dlog::info("sign-in: opening Spotify login page (loopback 127.0.0.1:8898)");
    stop_session();
    state.set(SpotifyState::Connecting);
    let pkce = auth::Pkce::new();
    let url = pkce.authorize_url();
    let finished = Rc::new(RefCell::new(false));

    // Detached and run from an engine task, so it outlives the sign-in button and has the
    // tokio runtime context `background` needs.
    Owner::detached().run(move || {
        let done = finished.clone();
        spawn(async move {
            let listener_pkce = pkce.clone();
            let token_task = background(async move {
                let listener = auth::bind_listener().await?;
                crate::debug_log::info("sign-in: listener bound, waiting for redirect");
                let code = auth::wait_for_code(listener, &listener_pkce.state).await?;
                crate::debug_log::info("sign-in: redirect received, exchanging code");
                auth::exchange_code(&code, &listener_pkce).await
            });
            let result = match token_task.await {
                Ok(result) => result,
                Err(_) => Err(auth::AuthError::Listener("task stopped".into())),
            };
            *done.borrow_mut() = true;
            close_auth_session();
            match result {
                Ok(tokens) => {
                    dlog::info("sign-in: access token received");
                    start_session(state, session::Login::Token(tokens.access_token));
                }
                Err(auth::AuthError::Denied(e)) if e == "access_denied" => state.set(SpotifyState::SignedOut),
                Err(e) => {
                    dlog::error(format!("sign-in failed: {e:?}"));
                    state.set(SpotifyState::Error(e.to_string()));
                }
            }
        });

        let page = open_auth_session(AuthSessionRequest::new(url, CALLBACK_SCHEME));
        spawn(async move {
            let outcome = page.await;
            dlog::info(format!("sign-in: auth page closed: {outcome:?}"));
            if *finished.borrow() {
                return;
            }
            let cancelled = matches!(outcome, Ok(AuthSessionOutcome::Cancelled));
            // Give the listener a moment in case the redirect is still arriving.
            timer(std::time::Duration::from_millis(800)).await;
            if *finished.borrow() {
                return;
            }
            state.set(if cancelled {
                SpotifyState::SignedOut
            } else {
                SpotifyState::Error("Could not open the Spotify sign-in page.".into())
            });
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
