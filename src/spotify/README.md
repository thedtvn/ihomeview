# Spotify backend (experimental)

Goal: show and control Spotify playing on **another** Spotify Connect device (e.g. a Windows PC with Premium) from the iPad, without moving playback to the iPad. Built on librespot (fork, see below) and Spotify's **undocumented** services. It can break whenever Spotify changes them, and using an unofficial client may violate Spotify's terms.

## Files

- `connect.rs`: observer device registration, cluster to `Playback` mapping, command bodies. Pure and unit-tested.
- `session.rs`: the live librespot `Session`: access point login, dealer, observer loop.
- `mod.rs`: the UI contract (`SpotifyState`, `Playback`, `SpotifyCommand`, `start`, `send`) and persistence.

## librespot fork

`Cargo.toml` uses the whole librespot workspace from [fantasybot-red/librespot](https://github.com/fantasybot-red/librespot) (`dev` = upstream `dev` + one commit). Upstream librespot identifies as iOS when built for iOS; Spotify's access points answer every login from that identity with `TryAnotherAP`, and login5 password login returns `INVALID_CREDENTIALS` even for correct passwords. With the macOS identity the same OAuth token logs in. Tested on 2026-11-19 with a standalone binary: iOS identity failed on every access point, Windows and macOS identities logged in.

## Flow

1. **Sign in (pairing).** librespot-oauth's `DeviceAuthClient` (OAuth device authorization, RFC 8628) with the Spotify for Desktop client id `65b708073fc0480ea92a077233ca87bd` and scope `streaming`. The iPad shows a short code; the user approves it at spotify.com/pair on a phone or computer while the app polls. No browser on the iPad and no local port (the in-app login page loaded blank on iPadOS 15).
2. **Access point.** The token logs the `Session` in (`Session::connect`). The access point returns a reusable credential (`session.auth_data()`).
3. **Persist.** Only the username, that stored credential (base64) and a random observer device id are stored in NSH app storage. Later launches log in with `AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS`. Sign out removes them.
4. **Observe.** Dealer listeners are registered before the dealer starts. When the connection id arrives, the app registers a hidden, non-playing device (`is_active = false`, `can_be_player = false`, `hidden = true`) and follows `ClusterUpdate`s.
5. **Control.** `POST /connect-state/v1/player/command/from/{our id}/to/{active id}` with `{"command":{"endpoint":"pause|resume|skip_next|skip_prev"}}`. It never transfers playback.

## Runtime note

librespot needs the tokio runtime context; NSH press handlers do not have it, and their owner is disposed when the screen changes. Pairing and the session therefore run from detached engine tasks (`Owner::detached().run(|| spawn(..))`) that call `background(..)`.
## Debugging

Hold the clock to open the debug log: every login step, errors (with librespot's error kind) and commands are listed there, newest first, with Copy.

## Not yet verified

That a hidden non-player device receives cluster updates; that the player-command endpoint accepts commands from it. Failures show an error on the music card and in the debug log; it never shows made-up playback.

## Build notes

- librespot `dev` pins `vergen = 9.0.6` itself.
- librespot uses `sysinfo` 0.36, whose Apple process code calls `libc::proc_pidinfo`. `libc` 0.2.190+ declares that function for macOS only, so `Cargo.toml` pins `libc = "=0.2.189"`. Pins live in `Cargo.toml` because `nsh package` resolves a fresh lockfile.
