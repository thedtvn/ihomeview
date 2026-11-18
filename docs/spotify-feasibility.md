# Spotify integration decision

## Required behavior

Spotify Premium plays on the Windows PC. ihomeview runs on the iPad with no desktop companion. Playback must not silently move to the iPad. The requested implementation library is librespot.

## Result of the first source-level investigation

**Not established.** Upstream librespot's public Connect controls describe its own player, not a general controller for a separate active Windows device. This is a limitation of the inspected public API, not proof that a reverse-engineered implementation is impossible.

Source observations (upstream `dev`, package version observed as 0.8.0; not a pinned build dependency):

- `connect/src/spirc.rs`: the Dealer handler for `hm://connect-state/v1/player/command` receives commands for the local player. The private request handler is not an outbound remote controller.
- `Spirc::play`, `pause`, `next` and related methods operate on the local player; `transfer` acquires playback locally.
- `core/src/spclient.rs`: Connect-state registration methods write the current session's device state. They do not provide a demonstrated remote-device enumeration and command API.
- `COMPILING.md` does not establish an iOS build or integration procedure.

References:

- https://github.com/librespot-org/librespot/blob/dev/connect/src/spirc.rs
- https://github.com/librespot-org/librespot/blob/dev/core/src/spclient.rs
- https://github.com/librespot-org/librespot/blob/dev/COMPILING.md

## First-build boundary

No credential collection, Connect player announcement, audio decoder, or playback transfer is enabled. A placeholder is not an integration. The first build must not expose working-looking controls for an unsupported backend. Adding an unused dependency would not satisfy the requested integration and would increase build/runtime risk.

## Evidence needed before enabling remote control

1. A pinned implementation supporting discovery of the actual Windows Connect device.
2. Explicit user authentication without embedded secrets or Spotify-password collection; persisted refresh credentials stored in Keychain.
3. Remote play/pause/skip/seek demonstrated against the PC without audio moving to the iPad.
4. Correct remote metadata/album-art/track-change updates, disconnect handling and logout.
5. An arm64 iOS 15 build and foreground lifecycle validation on the target iPad.

Using internal protocols would be a separate experimental integration, with service-compatibility and account-policy risks. The official Spotify Web API with OAuth PKCE is a possible alternative requiring separate approval and verification of Spotify developer-account restrictions; it is not silently substituted here.
