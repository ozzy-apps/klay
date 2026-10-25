#!/usr/bin/env bash
# Build dist/klay_<version>_<arch>.deb from the release binary.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
cargo build --release --offline --locked
version=$(target/release/klay --version | cut -d ' ' -f 2)
arch=$(dpkg --print-architecture)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
chmod 755 "$root"
install -Dm755 -s target/release/klay "$root/usr/bin/klay"
install -Dm644 packaging/klay.desktop "$root/usr/share/applications/klay.desktop"
install -Dm644 assets/icon/klay.svg "$root/usr/share/icons/hicolor/scalable/apps/klay.svg"
for size in 32 64 128 256 512; do
  install -Dm644 "assets/icon/klay-$size.png" "$root/usr/share/icons/hicolor/${size}x${size}/apps/klay.png"
done
install -Dm644 README.md "$root/usr/share/doc/klay/README.md"
install -Dm644 CHANGELOG.md "$root/usr/share/doc/klay/changelog"
gzip -9n "$root/usr/share/doc/klay/changelog"
size=$(du -sk "$root/usr" | cut -f1)
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: klay
Version: $version
Architecture: $arch
Maintainer: Ozzy <klay@ozzy.co.nl>
Installed-Size: $size
Depends: libc6 (>= 2.35), libvulkan1, libx11-6, libxkbcommon0
Recommends: mesa-vulkan-drivers | nvidia-driver, libwayland-client0, libxcursor1, libxi6, libxrandr2, curl
Section: graphics
Priority: optional
Homepage: https://github.com/ozzy-apps/klay
Description: Digital sculpting in the spirit of ZBrush
 Klay is a fast digital sculpting app with a ZBrush-style workspace:
 subdivision levels, Sculptris Pro, Live Boolean, ZRemesher, ZModeler,
 masking, polypaint and 66 brushes.
CONTROL
mkdir -p dist
out="dist/klay_${version}_${arch}.deb"
fakeroot dpkg-deb --build -Zxz "$root" "$out" >/dev/null
printf '%s\n' "$out"
