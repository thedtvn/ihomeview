# Spotify backend (experimental)

Goal: show and control Spotify playing on **another** Spotify Connect device (e.g. a Windows PC with Premium) from the iPad, without moving playback to the iPad. Built on librespot 0.8.0 (`librespot-core`, `librespot-oauth`, `librespot-protocol`; no playback/audio crates) and Spotify's **undocumented** Connect protocol. It can break whenever Spotify changes its services, and using an unofficial client may violate Spotify's terms.

## Files

- `auth.rs` — OAuth authorization code + PKCE (RFC 7636 S256) with librespot's public "keymaster" client id `65b708073fc0480ea92a077233ca87bd`, redirect `http://127.0.0.1:8898/login`, scope `streaming`. No client secret, no password.
- `connect.rs` — observer device registration, cluster → `Playback` mapping, command bodies. Pure and unit-tested.
- `session.rs` — the live librespot `Session` loop.
- `mod.rs` — the UI contract (`SpotifyState`, `Playback`, `SpotifyCommand`, `start`, `send`) and persistence.

## Flow

1. **Sign in.** The app binds a one-shot listener on `127.0.0.1:8898`, then opens Spotify's authorize page in an `ASWebAuthenticationSession` (NSH `open_auth_session`). Spotify redirects to the loopback URL; the listener checks `state`, answers with a short page, and the app closes the sheet and exchanges the code (+ PKCE verifier) at `https://accounts.spotify.com/api/token`.
2. **Persist.** Only the refresh token (`spotify.refresh_token`) and a random 40-hex observer device id (`spotify.observer_device_id`) are stored, in NSH app storage. Tokens are never logged. Sign out removes the token.
3. **Connect.** Each session refreshes the access token, creates `Session` with `client_id = keymaster` and the observer device id, registers Dealer listeners (`hm://pusher/v1/connections/`, `hm://connect-state/v1/cluster`) *before* `Session::connect(Credentials::with_access_token(..))`, then starts the Dealer.
4. **Observe.** When the connection id arrives, the app `PUT`s `/connect-state/v1/devices/{id}` with `member_type = CONNECT_STATE`, `put_state_reason = NEW_DEVICE`, `is_active = false`, `can_play = false`, capabilities `can_be_player = false`, `hidden = true`, `is_observable = true`. The reply is the current `Cluster`; later `ClusterUpdate`s replace it. On stop, it `PUT`s inactive.
5. **Show.** The active device (`cluster.active_device_id`, not ours) → name; `player_state.track.metadata` (`title`, `artist_name`, `album_title`, `image_*_url` as `spotify:image:<hex>` → `https://i.scdn.co/image/<hex>`); missing fields are resolved once per track with `SpClient::get_track_metadata`. Progress = `position_as_of_timestamp` + elapsed since `timestamp` while playing.
6. **Control.** `POST /connect-state/v1/player/command/from/{our id}/to/{active id}` with JSON `{"command":{"endpoint":"pause|resume|skip_next|skip_prev"}}` (endpoint names from librespot-core `dealer/protocol/request.rs`).

It never calls `transfer`, never sets `is_active = true` and never plays audio.

## Verified vs unverified

Verified offline (`cargo test`): PKCE (RFC 7636 test vector), authorize URL, callback/state parsing, loopback listener, cluster mapping, observer request, command JSON/paths.

**Not yet verified** (needs a live account on the iPad):

- that Spotify accepts the keymaster client's PKCE flow with the loopback redirect from an `ASWebAuthenticationSession` on iPadOS 15;
- that the token logs a librespot `Session` in when compiled for iOS (librespot sends iOS client-token/login5 platform data on `target_os = "ios"`);
- that a hidden non-player device receives cluster updates for other devices;
- that the player-command endpoint accepts commands from a hidden observer.

If any of these fails, the music card shows an error message; it never shows made-up playback.

## Build note

librespot-core 0.8.0's build script fails with `vergen` 9.1.0. `Cargo.lock` pins `vergen` to 9.0.6 (`cargo update -p vergen@9.1.0 --precise 9.0.6`).
