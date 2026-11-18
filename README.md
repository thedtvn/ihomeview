# ihomeview

A native, iPad-only desk dashboard built in Rust with [Native Shell](https://nsh.dev/).

## Target

- iPad Air 2 (`iPad5,4`), arm64, iPadOS 15.8.7.
- Install an unsigned IPA through TrollStore; no Apple Developer signing account.
- No browser renderer, localhost server, or PC/NAS companion at runtime.
- Private GitHub repository; macOS GitHub Actions builds use the owner's GitHub account and available runner allowance.

## First milestone

Native clock/date, focus timer, display preferences, and clearly labelled music/camera setup states. Live weather, calendar access, camera streams, and remote PC notifications are not implemented by the first milestone.

## Spotify boundary

The desired behavior is to **keep playback on the Windows PC** while controlling it from this iPad. Spotify Premium is available. The user requested librespot; it must not silently transfer playback to the iPad or substitute a desktop companion.

librespot is an unofficial Spotify client primarily designed to implement a Connect playback device. Its documented local-player commands do not establish general remote control of another active device. Remote playback integration must be proven before controls are presented as working. Until then, music is shown as unavailable/not connected, not simulated.

Never commit Spotify credentials, refresh tokens, camera passwords, or authentication callback data. OAuth login, if implemented, must be an explicit user action; no embedded client secret or collection of Spotify passwords.

## Runtime limitations

A standalone dashboard does not make the iPad an Apple Home hub, Windows monitor, or replacement for OS-wide notifications. Optional keep-awake behavior should apply only while the dashboard is visible and restore normal behavior afterward. Low CPU/RAM use is a goal, not a measured claim until verified on the target iPad.

The earlier HTML dashboard was not deployed. Python packages installed during the discarded localhost-hosting investigation are not required by this native app.

## Build

NSH CLI and crates are pinned to **0.6.0**. Rust dependencies are recorded in `Cargo.lock`.

Local logic/UI checks:

```sh
cargo test --locked --lib
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

On macOS with Xcode, the arm64 Rust target, and NSH 0.6.0:

```sh
bash scripts/build-ios.sh
```

The private GitHub Actions workflow uses the standard Apple Silicon `macos-15` runner, checks the NSH distribution checksum, tests the app, then packages the unsigned `.app` inside `Payload/` as `artifacts/ihomeview.ipa`. It uses no Apple signing identity, provisioning profile, or App Store credentials. The artifact is intended for TrollStore only—not normal iOS installation or TestFlight.

The packager checks iPad support, the arm64 executable, minimum OS metadata and Mach-O deployment target; it emits a SHA-256 file and a JSON evidence report. Its unit tests use synthetic fixtures and mocked macOS tools, not a real IPA. A successful package check does not prove the app launches on iPadOS 15.

## Build status

Native views compile on the Windows desktop. `cargo test --locked --lib` passes six tests, including headless checks for the honest music/camera states and timer reset. No iOS build or device installation has been verified yet.
