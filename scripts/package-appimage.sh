#!/usr/bin/env bash
# Build dist/Klay-<version>-<arch>.AppImage from the AppDir that package-linux.sh makes.
# Needs appimagetool (https://github.com/AppImage/appimagetool): on the PATH, or set
# APPIMAGETOOL to its file.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
tool=${APPIMAGETOOL:-$(command -v appimagetool || true)}
if [ -z "$tool" ]; then
  echo "appimagetool not found: put it on the PATH or set APPIMAGETOOL" >&2
  exit 1
fi
appdir=$(scripts/package-linux.sh | head -n 1)
version=$(target/release/klay --version | cut -d ' ' -f 2)
arch=$(uname -m)
out="dist/Klay-$version-$arch.AppImage"
ARCH="$arch" "$tool" --no-appstream "$appdir" "$out" >/dev/null
printf '%s\n' "$out"
