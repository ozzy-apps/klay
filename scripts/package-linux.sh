#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
cargo build --release --offline --locked
klay_version=$(target/release/klay --version | cut -d ' ' -f 2)
klay_arch=$(uname -m)
klay_bundle="$PWD/dist/Klay-$klay_version-$klay_arch.AppDir"
mkdir -p "$klay_bundle/usr/bin" "$klay_bundle/usr/share/icons/hicolor/scalable/apps" "$klay_bundle/usr/share/applications"
install -m 755 target/release/klay "$klay_bundle/usr/bin/klay"
install -m 755 packaging/AppRun "$klay_bundle/AppRun"
install -m 644 assets/icon/klay.svg "$klay_bundle/klay.svg"
install -m 644 assets/icon/klay.svg "$klay_bundle/usr/share/icons/hicolor/scalable/apps/klay.svg"
install -m 644 packaging/klay.desktop "$klay_bundle/klay.desktop"
install -m 644 packaging/klay.desktop "$klay_bundle/usr/share/applications/klay.desktop"
ln -sfn klay.svg "$klay_bundle/.DirIcon"
install -m 644 CHANGELOG.md "$klay_bundle/CHANGELOG.md"
install -m 644 README.md "$klay_bundle/README.md"
printf '%s\n' "$klay_bundle"
