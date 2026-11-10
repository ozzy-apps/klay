#!/usr/bin/env bash
# Windows x86-64 zip, cross-compiled on Linux: needs `rustup target add x86_64-pc-windows-gnu`
# and mingw-w64 (x86_64-w64-mingw32-gcc). The exe links only Windows system DLLs.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
target=x86_64-pc-windows-gnu
cargo build --release --offline --locked --target "$target"
klay_version=$(grep -m1 '^version' Cargo.toml | cut -d '"' -f 2)
name="Klay-$klay_version-x86_64-windows"
stage="$PWD/dist/$name"
rm -rf "$stage" "$stage.zip"
mkdir -p "$stage"
x86_64-w64-mingw32-strip -o "$stage/klay.exe" "target/$target/release/klay.exe"
install -m 644 README.md LICENSE CHANGELOG.md "$stage/"
(cd dist && zip -qr9 "$name.zip" "$name")
printf '%s\n' "$stage.zip"
