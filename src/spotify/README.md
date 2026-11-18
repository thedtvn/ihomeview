# Spotify backend (experimental)

Goal: show and control Spotify playing on **another** Spotify Connect device (e.g. a Windows PC with Premium) from the iPad, without moving playback to the iPad. Built on librespot from GitHub `dev` (commit `15e02e8`, pinned in `Cargo.toml`; `librespot-core` and `librespot-protocol`; no playback/audio crates) and Spotify's **undocumented** services. It can break whenever Spotify changes them, and using an unofficial client may violate Spotify's terms.

## Files

- `connect.rs`: observer device registration, cluster to `Playback` mapping, command bodies. Pure and unit-tested.
- `session.rs`: the live librespot `Session`: login5, access point login, dealer, observer loop.
- `mod.rs`: the UI contract (`SpotifyState`, `Playback`, `SpotifyCommand`, `start`, `send`, `sign_in_with_password`) and persistence.

## Flow

1. **Sign in (login5).** The music card shows an email/username + password form. On submit, the session (built with librespot's default `SessionConfig` for `target_os = "ios"`, i.e. the iOS client id and an iOS user agent) calls `session.login5().login(user, password)` against `https://login5.spotify.com/v3/login`, solving the hashcash challenge if Spotify sends one. Code (email/SMS) challenges are not supported and show an error.
2. **Access point.** The login5 access token logs the `Session` into the access point (`Session::connect`). The access point returns a reusable credential (`session.auth_data()`).
3. **Persist.** Only the username, that stored credential (base64, `spotify.stored_credentials`) and a random 40-hex observer device id are stored, in NSH app storage. The password is never stored or logged. Later launches log in with `AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS`. Sign out, or a "bad credentials" reply, removes them.
4. **Observe.** Dealer listeners (`hm://pusher/v1/connections/`, `hm://connect-state/v1/cluster`) are registered before the dealer starts. When the connection id arrives, the app `PUT`s `/connect-state/v1/devices/{id}` as a hidden, non-playing device (`is_active = false`, `can_be_player = false`, `hidden = true`). The reply is the current `Cluster`; later `ClusterUpdate`s replace it.
5. **Show.** The active device's name and `player_state.track.metadata`; missing fields are resolved once per track with `SpClient::get_track_metadata`.
6. **Control.** `POST /connect-state/v1/player/command/from/{our id}/to/{active id}` with `{"command":{"endpoint":"pause|resume|skip_next|skip_prev"}}`.

It never calls `transfer`, never sets `is_active = true` and never plays audio.

## Runtime note

librespot calls `tokio::spawn` / `Handle::current()`, which need the tokio runtime's context. NSH press handlers do not run in it (`background` panicked with "there is no reactor running"), so `mod.rs` starts the session with `background(..)` from inside an engine task (`spawn`), which NSH polls in the runtime's context.

## Debugging

Hold the clock to open the debug log: every login step, errors (with librespot's error kind) and commands are listed there, newest first, with Copy.

## Not yet verified

That login5 password login from the iPad is accepted without a code challenge; that a hidden non-player device receives cluster updates; that the player-command endpoint accepts commands from it. Failures show an error on the music card and in the debug log; it never shows made-up playback.

## Build notes

- librespot `dev` pins `vergen = 9.0.6` itself.
- librespot uses `sysinfo` 0.36, whose Apple process code calls `libc::proc_pidinfo`. `libc` 0.2.190+ declares that function for macOS only, so `Cargo.toml` pins `libc = "=0.2.189"`. Pins live in `Cargo.toml` because `nsh package` resolves a fresh lockfile.
