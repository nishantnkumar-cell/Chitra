#!/bin/sh
# Installs Chitra for the current user (no root needed):
#   ./install.sh               into ~/.local
#   ./install.sh --uninstall   removes it again
# Set PREFIX to install elsewhere, e.g. sudo PREFIX=/usr/local ./install.sh
set -eu

ID=io.github.nishantnkumar_cell.Chitra
PREFIX=${PREFIX:-$HOME/.local}
HERE=$(cd "$(dirname "$0")" && pwd)

refresh() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$PREFIX/share/applications" || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true
    fi
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$PREFIX/bin/chitra" "$PREFIX/bin/chitra-cli" \
        "$PREFIX/share/applications/$ID.desktop" \
        "$PREFIX/share/metainfo/$ID.metainfo.xml"
    rm -f "$PREFIX"/share/icons/hicolor/*/apps/"$ID".png
    refresh
    echo "Chitra removed from $PREFIX"
    exit 0
fi

install -Dm755 "$HERE/chitra" "$PREFIX/bin/chitra"
install -Dm755 "$HERE/chitra-cli" "$PREFIX/bin/chitra-cli"
# Use the full path so the menu entry works even if $PREFIX/bin is not on PATH.
mkdir -p "$PREFIX/share/applications"
sed "s|^Exec=chitra |Exec=$PREFIX/bin/chitra |" "$HERE/share/applications/$ID.desktop" \
    > "$PREFIX/share/applications/$ID.desktop"
install -Dm644 "$HERE/share/metainfo/$ID.metainfo.xml" "$PREFIX/share/metainfo/$ID.metainfo.xml"
for icon in "$HERE"/share/icons/hicolor/*/apps/"$ID".png; do
    size=$(basename "$(dirname "$(dirname "$icon")")")
    install -Dm644 "$icon" "$PREFIX/share/icons/hicolor/$size/apps/$ID.png"
done
refresh
echo "Chitra installed in $PREFIX. It is in your app menu, and Open With lists it for photos."
