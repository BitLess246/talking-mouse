#!/bin/sh
# Installs Talking Mouse from the release tarball into ~/.local (or $PREFIX).
#   ./install.sh               install
#   ./install.sh --uninstall   remove
#   PREFIX=/usr/local sudo -E ./install.sh   system-wide
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
here=$(cd "$(dirname "$0")" && pwd)
apps="$PREFIX/share/applications"
icons="$PREFIX/share/icons/hicolor"

refresh() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$apps" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$PREFIX/bin/talking-mouse" "$PREFIX/bin/talking-mouse-editor" "$apps/talking-mouse.desktop"
    for size in 256 512; do rm -f "$icons/${size}x${size}/apps/talking-mouse.png"; done
    refresh
    echo "Talking Mouse removed from $PREFIX"
    exit 0
fi

install -Dm755 "$here/talking-mouse" "$PREFIX/bin/talking-mouse"
install -Dm755 "$here/talking-mouse-editor" "$PREFIX/bin/talking-mouse-editor"
mkdir -p "$apps"
# Launchers don't always search ~/.local/bin, so point at the binary directly.
sed "s|^Exec=talking-mouse-editor|Exec=$PREFIX/bin/talking-mouse-editor|" \
    "$here/talking-mouse.desktop" > "$apps/talking-mouse.desktop"
chmod 644 "$apps/talking-mouse.desktop"
for size in 256 512; do
    install -Dm644 "$here/icons/icon-$size.png" "$icons/${size}x${size}/apps/talking-mouse.png"
done
refresh

echo "Installed Talking Mouse to $PREFIX"
echo "  Launch it from your app menu, or run: talking-mouse-editor"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "  Note: $PREFIX/bin is not on your PATH; add it to use the 'talking-mouse' CLI." ;;
esac
if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
    echo "  Warning: ffmpeg is not installed. Install it first, e.g. 'sudo apt install ffmpeg'."
fi
