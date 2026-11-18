# ihomeview

A native desk dashboard for an old iPad, written in Rust with [Native Shell](https://nsh.dev/) (UIKit views, no WebView or JavaScript runtime).

- Large clock and date, 12/24-hour switch and night dimming.
- A focus timer that stays hidden until you open it.
- A music card for Spotify (experimental, see below).

## Requirements

- An arm64 iPad on iPadOS 15 or newer. Developed on an iPad Air 2 (`iPad5,4`) with iPadOS 15.8.7.
- [TrollStore](https://github.com/opa334/TrollStore) to install the unsigned IPA. No Apple Developer account is used.

## Install

Download `ihomeview-ipad-arm64` from the latest successful [GitHub Actions run](../../actions/workflows/ios.yml), unzip it, check `ihomeview.ipa` against `ihomeview.ipa.sha256`, and open the IPA with TrollStore.

## Spotify (experimental)

The music card is meant to show and control Spotify playing on **another** device, such as a Windows PC with Spotify Premium, without moving playback to the iPad. It uses [librespot](https://github.com/librespot-org/librespot), an unofficial Spotify client, and Spotify's undocumented Spotify Connect messages:

- It may stop working whenever Spotify changes its internal services.
- Using an unofficial client may break Spotify's terms of service; use it at your own risk.
- Sign-in happens on Spotify's own login page. The app never asks for or stores your password; only a refresh token is stored on the iPad.

Status and evidence: [docs/spotify-feasibility.md](docs/spotify-feasibility.md) and the [librespot spike](spikes/librespot-remote/README.md).

## Not included

Camera streams, weather, calendar and PC notifications are not implemented. The iPad does not become an Apple Home hub or a Windows display.

## Build

Native Shell CLI and crates are pinned to **0.6.0**; Rust dependencies are locked in `Cargo.lock`.

Checks that run on any desktop:

```sh
cargo test --locked --lib
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

The IPA must be built on macOS with Xcode, Rust's `aarch64-apple-ios` target and NSH 0.6.0:

```sh
bash scripts/build-ios.sh
```

GitHub Actions (`.github/workflows/ios.yml`) runs the same build on the `macos-15` Apple Silicon runner, verifies the NSH download checksum, runs the tests and uploads the unsigned IPA with its SHA-256 and a JSON build report. The packager only checks the IPA's metadata, architecture and minimum OS; a successful build does not prove the app runs on a particular device.

## License

[MIT](LICENSE) © The DT
