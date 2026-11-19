#!/bin/sh
# Packs the Linux release build into dist/chitra_<version>_amd64.deb for
# Debian and Ubuntu. Run after cargo build --release. Needs dpkg-dev.
set -eu

ID=io.github.nishantnkumar_cell.Chitra
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ROOT=dist/deb/chitra
rm -rf dist/deb
mkdir -p "$ROOT/DEBIAN"

install -Dm755 target/release/chitra "$ROOT/usr/bin/chitra"
install -Dm755 target/release/chitra-cli "$ROOT/usr/bin/chitra-cli"
install -Dm644 "packaging/linux/$ID.desktop" "$ROOT/usr/share/applications/$ID.desktop"
install -Dm644 "packaging/linux/$ID.metainfo.xml" "$ROOT/usr/share/metainfo/$ID.metainfo.xml"
mkdir -p "$ROOT/usr/share"
cp -r packaging/linux/icons "$ROOT/usr/share/"
DOC="$ROOT/usr/share/doc/chitra"
install -Dm644 installer/NOTICE.txt "$DOC/copyright"
cat installer/LICENSE-AGPL-3.0.txt >> "$DOC/copyright"
find "$ROOT/usr" -type d -exec chmod 755 {} +

# Library dependencies of the binaries, worked out by dpkg-shlibdeps (it
# needs a debian/control to run from).
WORK=$(mktemp -d)
mkdir -p "$WORK/debian"
printf 'Source: chitra\n\nPackage: chitra\nArchitecture: amd64\n' > "$WORK/debian/control"
BINS="$(pwd)/$ROOT/usr/bin/chitra $(pwd)/$ROOT/usr/bin/chitra-cli"
DEPENDS=$(cd "$WORK" && dpkg-shlibdeps -O $BINS 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
rm -rf "$WORK"

SIZE=$(du -sk "$ROOT/usr" | cut -f1)
cat > "$ROOT/DEBIAN/control" <<EOF
Package: chitra
Version: $VERSION
Architecture: amd64
Maintainer: Nishant <noreply@github.com>
Installed-Size: $SIZE
Depends: $DEPENDS
Recommends: curl, gstreamer1.0-plugins-good, gstreamer1.0-libav
Section: graphics
Priority: optional
Homepage: https://github.com/nishantnkumar-cell/Chitra
Description: Fast, small, memory-safe photo viewer and batch converter
 Chitra opens JPEG, JPEG XL, PNG, GIF, BMP, WebP, AVIF and HEIC photos and
 phone videos, converts between formats in batches, upscales and sharpens
 (with optional AI models), makes passport photos and prints.
EOF

OUT="dist/chitra_${VERSION}_amd64.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$OUT"
rm -rf dist/deb
echo "$OUT"
