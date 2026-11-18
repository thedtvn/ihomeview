#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]] || { echo 'iOS packaging requires macOS/Xcode.' >&2; exit 1; }
export IPHONEOS_DEPLOYMENT_TARGET=15.0
rustup target add aarch64-apple-ios
nsh --version
# -v (or NSH_VERBOSE=1) streams cargo/xcode output live instead of only on failure.
nsh -v package --target ios --out build/ios
python3 scripts/package-ipa.py build/ios artifacts/ihomeview.ipa
