#!/bin/sh
# Packs the Linux release build into dist/linux (unpacked, used by the Flatpak)
# and dist/chtra-<version>-linux-x86_64.tar.gz. Run after cargo build --release.
set -eu

ID=io.github.nishantnkumar_cell.Chtra
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
OUT=dist/linux
rm -rf "$OUT"
mkdir -p "$OUT/share/applications" "$OUT/share/metainfo"

install -m755 target/release/chtra target/release/chtra-cli packaging/linux/install.sh "$OUT/"
install -m644 "packaging/linux/$ID.desktop" "$OUT/share/applications/"
install -m644 "packaging/linux/$ID.metainfo.xml" "$OUT/share/metainfo/"
cp -r packaging/linux/icons "$OUT/share/"
install -m644 installer/LICENSE-AGPL-3.0.txt installer/NOTICE.txt "$OUT/"

NAME="chtra-$VERSION-linux-x86_64"
rm -rf "dist/$NAME" "dist/$NAME.tar.gz"
cp -r "$OUT" "dist/$NAME"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "dist/$NAME"
echo "dist/$NAME.tar.gz"
