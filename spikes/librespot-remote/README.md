# Experimental librespot remote spike

## Verdict

**Not proven feasible.** Published librespot 0.8.0 exposes low-level building blocks, but not a demonstrated API to list another device's playback or send it commands without registering a device. We did not test Spotify network behavior. Do not wire working controls into the app based on this spike. Spotify internals are undocumented and unstable, and account-policy risk remains.

The CLI refreshes an access token and connects a Session when invoked without arguments, then reports that device listing/playback are unsupported. Command arguments are explicitly rejected before any network access. It never registers a Connect device or transfers playback. Only `--help` was run. Set `SPOTIFY_REFRESH_TOKEN`, `SPOTIFY_CLIENT_ID`, and `SPOTIFY_REDIRECT_URI` for the untested session path; never put tokens in source or logs.

## Versions and builds

Pinned `librespot-core`, `librespot-oauth`, `librespot-protocol` = 0.8.0 (crates.io release metadata: https://crates.io/api/v1/crates/librespot-core/0.8.0 ; MSRV 1.85). OAuth requires exactly one TLS family (`librespot-oauth-0.8.0/src/lib.rs:31-50`); selected native-tls. Core already depends on OAuth; protocol is explicitly included for future investigation. No connect, playback, or audio crates. This is a minimal chosen TLS family, not a proof of the smallest possible dependency graph.

Keep this crate's Cargo.lock: the orchestrator ran `cargo update -p vergen@9.1.0 --precise 9.0.6` after the original failure. Any app integration must apply the same pin to its own lockfile: vergen 9.1.0 resolved vergen-lib 9.1.0 while vergen-gitcl 1.0.8 uses vergen-lib 0.1.6, breaking the core build script. This change does not alter the app lockfile.

Commands in this directory:

* Original `cargo check`: exit 1, `error[E0277]: the trait bound \`vergen::feature::build::Build: vergen_lib::entries::Add\` is not satisfied`; `error: could not compile \`librespot-core\` (build script) due to 1 previous error`.
* Orchestrator-reported after pin: `cargo check` exit 0, `Finished \`dev\` profile ... EXIT=0`; not independently run at that point by this agent.
* `rustup target list --installed`: exit 0, exact stdout: `x86_64-pc-windows-msvc`.
* After CLI change, `cargo check`: exit 0, final output `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 9.74s`.
* `cargo run -- --help`: exit 0; after compilation output: `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1m 00s`, `Running \`target\debug\librespot-remote-spike.exe --help\``, then:

```text
librespot-remote-spike

Usage: librespot-remote-spike [--command pause|play|next|prev --device ID]

Set SPOTIFY_REFRESH_TOKEN, SPOTIFY_CLIENT_ID and SPOTIFY_REDIRECT_URI.
Device listing, playback state and remote commands are not implemented: their
undocumented Connect protocol requirements have not been established.
No playback device is registered or playback transferred.
```

`cargo check --target aarch64-apple-ios` was not run: the target is not installed. Install it via rustup and check on macOS with Xcode/iOS SDK; a complete iOS build/link requires the Apple SDK and linker, and cross-compiling native dependencies from Windows is not established. Native TLS uses Apple's Security.framework on Apple targets. The lockfile includes `cc`, `openssl`, and `security-framework` (target-specific lockfile entries do not imply all build for iOS); C/compiler and system framework requirements need verification on macOS. No iOS success is claimed.

## Authentication

Registry source paths below are relative to `~/.cargo/registry/src/*/` and refer to 0.8.0.

`librespot-oauth-0.8.0/src/lib.rs:1-12,169-222,233-255,284-360` implements authorization code + PKCE, with a blocking HTTP callback listener for HTTP URIs with ports, otherwise stdin; no device-code flow is implemented. `OAuthClientBuilder::new(client_id: &str, redirect_uri: &str, scopes: Vec<&str>)` takes caller-supplied values (`:363-425`); there is no universal hardcoded client ID or fixed scope list in that builder. The example `examples/oauth_async.rs:5-6,23-32` uses client ID `65b708073fc0480ea92a077233ca87bd`, redirect `http://127.0.0.1:8898/login`, scope `streaming`, or caller-provided values. Core's OS defaults are different for iOS (`librespot-core-0.8.0/src/config.rs:6-8,26-45`). Core lists known scopes including `user-read-playback-state`, `user-read-currently-playing`, `user-modify-playback-state`, and `streaming` (`src/token.rs:3-9`); this list does not establish authorization for private Connect APIs or third-party IDs.

`OAuthToken.refresh_token` is returned at `librespot-oauth-0.8.0/src/lib.rs:125-138,258-281`; `refresh_token_async(&self, refresh_token: &str)` is at `:348-360`. Securely store it in iOS Keychain, replace it if refresh returns a nonempty rotated token, and retain the old one when no new token is returned (this crate maps missing refresh token to empty string). Avoid trace logs: `:262` logs the token response. Never collect passwords or ship a client secret.

For iPad, use ASWebAuthenticationSession/open_auth_session with a registered redirect and PKCE, retaining and validating OAuth state and verifier, and use a client ID/redirect Spotify actually permits. This requires a custom/adapted authorization flow: this crate's URL generation and callback helpers are private, it prints the URL and discards the generated CSRF state (`:237-255`), and its async login still synchronously waits for listener/stdin (`:326-345`). Do not assume a custom scheme or loopback is accepted without testing Spotify registration and iPad behavior. Existing `AwaitingApproval { code, url }` is device-code-shaped, not a natural PKCE callback interface.

## Connect investigation

* `librespot-core-0.8.0/src/session.rs:206-210,242-259,318-325` exposes `Session::connect(&self, credentials: Credentials, store_credentials: bool)` and access to `dealer()`/`spclient()`. `authentication.rs:61` provides `Credentials::with_access_token`. Session login alone is not evidence of Connect cluster access.
* `src/spclient.rs:378-435` exposes generic `request_with_protobuf`, `request_as_json`, and `request`; `:487-518` adds login5 bearer auth and attempts a client token. This allows hand-built requests, not evidence that Spotify accepts a guessed route or payload. The only explicit connect-state calls are `put_connect_state_request(&self, state: &PutStateRequest)`, `delete_connect_state_request(&self)`, `put_connect_state_inactive(&self, notify: bool)` (`:552-577`), plus `transfer` (`:941-950`), which must not be used for this goal. There is **no named account device-list or player-command method**, and no `/connect-state/v1/player/command/from/{from}/to/{to}` string in this source. That candidate endpoint could be constructed via generic request, but method, headers, body, permissions and acceptance are unverified; do not send guessed commands.
* `src/dealer/manager.rs:84-111,147-166` offers `add_listen_for(&self, url: impl Into<String>) -> Result<Subscription, Error>`, `listen_for<T>(...) -> Result<BoxedStreamResult<T>, Error>`, and `start(&self) -> Result<(), Error>` for the authenticated Dealer websocket. Core itself does not identify a cluster subscription topic or show a successful remote-only subscription. State updates may require registration; source does not establish whether a PUT is necessary, or whether a hidden nonplaying member receives updates or can command others.
* `librespot-protocol-0.8.0/proto/connect.proto:14-56,62-89,98-121` defines `ClusterUpdate`, `Cluster` (active_device_id, player_state, device map), `Device`, `PutStateRequest` (including `is_active`), `DeviceInfo.can_play`, and `Capabilities.can_be_player`/`hidden`. `proto/player.proto:10-62` defines player state, URI, metadata, duration, position, playing/paused. This suggests a possible investigation: subscribe to cluster updates, parse `ClusterUpdate`, resolve missing metadata using `SpClient::get_track_metadata` (`spclient.rs:580-625`), and if required construct `PutStateRequest` with nonactive `Device` / `DeviceInfo` and capabilities marked hidden/nonplayer. The fields exist; **neither server behavior nor invisibility is proven**, and PUT would still register a device. Source does not establish the exact command request type or serialization; pretending to specify one would be misleading.

## Integration estimate

Not production-ready; discovery and real-device testing dominate and cannot be reliably estimated until the command/cluster and nonregistration questions are resolved. If proven, expect a separate significant integration effort: iOS OAuth callback and Keychain, session lifecycle/reconnect, cluster/metadata mapping to `src/spotify/mod.rs` `Playback`, progress and capability updates, and command channel matching `start`/`send`. The fixed interface's device-code `AwaitingApproval` does not represent an ASWebAuthenticationSession callback; resolve that mismatch without misrepresenting a device-code flow. Keep `Unavailable`/`can_control = false` unless the no-transfer remote path is verified on the target iPad and Windows Spotify.
