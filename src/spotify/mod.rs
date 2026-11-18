//! Spotify Connect boundary between the dashboard and the experimental librespot backend.
//!
//! The UI only depends on [`SpotifyState`], [`Playback`], [`SpotifyCommand`], [`start`] and
//! [`send`]. The backend (`auth`, `connect`, `session`) signs in with OAuth PKCE, observes the
//! account's Spotify Connect devices without becoming a player, and remote-controls the active
//! device. See `README.md` in this directory.
use std::{cell::RefCell, rc::Rc};

use nsh_core::{
    auth_session::{AuthSessionOutcome, AuthSessionRequest},
    prelude::*,
    storage::Storage,
};
use tokio::sync::mpsc;

pub mod auth;
pub mod connect;
mod session;

/// Connection and playback state shown by the dashboard.
#[derive(Clone, Debug, PartialEq)]
pub enum SpotifyState {
    /// The backend is not available in this build, with a short reason for the user.
    Unavailable(String),
    /// No saved Spotify login; the user can start sign-in.
    SignedOut,
    /// Device sign-in started: the user enters `code` at `url` on another device.
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
    SignIn,
    SignOut,
    PlayPause,
    Next,
    Previous,
}

const KEY_REFRESH: &str = "spotify.refresh_token";
const KEY_DEVICE: &str = "spotify.observer_device_id";
const CALLBACK_SCHEME: &str = "ihomeview";

thread_local! {
    /// The running session's request channel (engine thread only).
    static SESSION: RefCell<Option<mpsc::UnboundedSender<session::Request>>> = const { RefCell::new(None) };
}

fn observer_device_id() -> String {
    if let Some(id) = Storage::get_item(KEY_DEVICE).filter(|id| !id.is_empty()) {
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

/// Starts a session from the stored refresh token and pumps its events into `state`.
fn start_session(state: Signal<SpotifyState>, refresh_token: String) {
    crate::debug_log::info("spotify: starting session from saved login");
    stop_session();
    state.set(SpotifyState::Connecting);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    SESSION.with(|s| *s.borrow_mut() = Some(request_tx.clone()));
    let device_id = observer_device_id();
    background(session::run(refresh_token, device_id, event_tx, request_rx));

    spawn(async move {
        while let Some(event) = event_rx.recv().await {
            // Ignore events from a session that has since been replaced or stopped.
            let current = SESSION.with(|s| s.borrow().as_ref().is_some_and(|tx| tx.same_channel(&request_tx)));
            if !current {
                break;
            }
            match event {
                session::Event::RefreshToken(token) => {
                    let _ = Storage::set_item(KEY_REFRESH, token);
                }
                session::Event::Playback(playback) => {
                    if !matches!(state.get_untracked(), SpotifyState::Connected(_)) {
                        crate::debug_log::info("spotify: connected");
                    }
                    state.set(SpotifyState::Connected(playback))
                }
                session::Event::Error(message) => {
                    crate::debug_log::warn(format!("spotify: {message}"));
                    // Keep showing playback; surface command errors without dropping the session.
                    if !matches!(state.get_untracked(), SpotifyState::Connected(_)) {
                        state.set(SpotifyState::Error(message));
                    } else {
                        log::warn!("spotify: {message}");
                    }
                }
                session::Event::Ended { message, signed_out } => {
                    SESSION.with(|s| *s.borrow_mut() = None);
                    if signed_out {
                        Storage::remove_item(KEY_REFRESH);
                    }
                    state.set(match message {
                        Some(m) => SpotifyState::Error(m),
                        None => SpotifyState::SignedOut,
                    });
                    break;
                }
            }
        }
    });
}

/// Runs the OAuth sign-in: loopback listener + Spotify's page in an auth session.
fn sign_in(state: Signal<SpotifyState>) {
    crate::debug_log::info("sign-in: starting (loopback 127.0.0.1:8898 + auth session)");
    stop_session();
    state.set(SpotifyState::Connecting);
    let pkce = auth::Pkce::new();
    let url = pkce.authorize_url();
    let finished = Rc::new(RefCell::new(false));

    // The listener runs on the runtime threads; the page in an ASWebAuthenticationSession.
    let listener_pkce = pkce.clone();
    let token_task = background(async move {
        let listener = auth::bind_listener().await?;
        crate::debug_log::info("sign-in: listener bound, waiting for redirect");
        let code = auth::wait_for_code(listener, &listener_pkce.state).await?;
        crate::debug_log::info("sign-in: redirect received, exchanging code");
        auth::exchange_code(&code, &listener_pkce).await
    });

    let done = finished.clone();
    spawn(async move {
        let result = match token_task.await {
            Ok(result) => result,
            Err(_) => Err(auth::AuthError::Listener("task stopped".into())),
        };
        *done.borrow_mut() = true;
        close_auth_session();
        match &result {
            Ok(t) => crate::debug_log::info(format!(
                "sign-in: token ok (refresh token: {})",
                t.refresh_token.as_deref().is_some_and(|r| !r.is_empty())
            )),
            Err(e) => crate::debug_log::error(format!("sign-in failed: {e:?}")),
        }
        match result {
            Ok(tokens) => match tokens.refresh_token.filter(|t| !t.is_empty()) {
                Some(refresh) => {
                    let _ = Storage::set_item(KEY_REFRESH, refresh.clone());
                    start_session(state, refresh);
                }
                None => state.set(SpotifyState::Error("Spotify did not return a login token.".into())),
            },
            Err(auth::AuthError::Denied(e)) if e == "access_denied" => state.set(SpotifyState::SignedOut),
            Err(e) => state.set(SpotifyState::Error(e.to_string())),
        }
    });

    let page = open_auth_session(AuthSessionRequest::new(url, CALLBACK_SCHEME));
    spawn(async move {
        let outcome = page.await;
        crate::debug_log::info(format!("sign-in: auth page closed: {outcome:?}"));
        if *finished.borrow() {
            return;
        }
        // The page closed without the redirect reaching the app.
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
}

/// Starts the backend once for the dashboard, restoring a saved login when one exists.
pub fn start(state: Signal<SpotifyState>) {
    match Storage::get_item(KEY_REFRESH).filter(|t| !t.is_empty()) {
        Some(token) => start_session(state, token),
        None => state.set(SpotifyState::SignedOut),
    }
}

/// Sends a user action to the backend.
pub fn send(state: Signal<SpotifyState>, command: SpotifyCommand) {
    match command {
        SpotifyCommand::SignIn => sign_in(state),
        SpotifyCommand::SignOut => {
            stop_session();
            Storage::remove_item(KEY_REFRESH);
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
