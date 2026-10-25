#!/usr/bin/env bash
# Build dist/Klay-<version>-<arch>.flatpak from the release binary, on the Freedesktop
# runtime (installed from Flathub), with flatpak's own build commands.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
app=nl.co.ozzy.Klay
runtime_version=25.08
cargo build --release --offline --locked
version=$(target/release/klay --version | cut -d ' ' -f 2)
arch=$(uname -m)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
build="$work/build"
flatpak build-init "$build" "$app" org.freedesktop.Platform org.freedesktop.Platform "$runtime_version" >/dev/null
files="$build/files"
install -Dm755 -s target/release/klay "$files/bin/klay"
sed -e "s/^Icon=.*/Icon=$app/" packaging/klay.desktop > "$work/$app.desktop"
install -Dm644 "$work/$app.desktop" "$files/share/applications/$app.desktop"
install -Dm644 assets/icon/klay.svg "$files/share/icons/hicolor/scalable/apps/$app.svg"
for size in 64 128 256 512; do
  install -Dm644 "assets/icon/klay-$size.png" "$files/share/icons/hicolor/${size}x${size}/apps/$app.png"
done
install -Dm644 "packaging/$app.metainfo.xml" "$files/share/metainfo/$app.metainfo.xml"
# Wayland or X11, the GPU, projects in the home folder, and the network for update checks.
flatpak build-finish "$build" --command=klay \
  --socket=wayland --socket=fallback-x11 --share=ipc --device=dri \
  --filesystem=home --share=network >/dev/null
flatpak build-export --arch="$arch" "$work/repo" "$build" stable >/dev/null
mkdir -p dist
out="dist/Klay-$version-$arch.flatpak"
flatpak build-bundle --arch="$arch" --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
  "$work/repo" "$out" "$app" stable
printf '%s\n' "$out"
