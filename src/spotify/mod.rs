//! Spotify Connect boundary between the dashboard and the experimental librespot backend.
//!
//! The UI only depends on [`SpotifyState`], [`Playback`], [`SpotifyCommand`], [`start`] and
//! [`send`]. Until the backend is implemented and verified, [`start`] reports
//! [`SpotifyState::Unavailable`] and commands do nothing; the UI must not present controls as
//! working in that state.
use nsh_core::prelude::*;

/// Connection and playback state shown by the dashboard.
#[derive(Clone, Debug, PartialEq)]
pub enum SpotifyState {
    /// The backend is not available in this build, with a short reason for the user.
    Unavailable(String),
    /// No saved Spotify login; the user can start sign-in.
    SignedOut,
    /// Device sign-in started: the user enters `code` at `url` on another device.
    AwaitingApproval { code: String, url: String },
    /// Signed in; connecting to Spotify or restoring a saved login.
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

/// Starts the backend once for the dashboard, restoring a saved login when one exists.
pub fn start(state: Signal<SpotifyState>) {
    state.set(SpotifyState::Unavailable(
        "Spotify sign-in is not available in this build yet.".into(),
    ));
}

/// Sends a user action to the backend. Ignored while the backend is unavailable.
pub fn send(state: Signal<SpotifyState>, command: SpotifyCommand) {
    let _ = (state, command);
}
