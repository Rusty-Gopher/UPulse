#!/usr/bin/env bash
# Install UPulse into your user environment (no root required):
#   - binary        -> ~/.local/bin/upulse
#   - icon          -> ~/.local/share/icons/hicolor/scalable/apps/upulse.svg
#   - app launcher  -> ~/.local/share/applications/upulse.desktop
#   - desktop icon  -> ~/Desktop/upulse.desktop   (trusted, double-clickable)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "==> Building release binary…"
cargo build --release --manifest-path "$here/Cargo.toml"

bin_dir="$HOME/.local/bin"
app_dir="$HOME/.local/share/applications"
icon_dir="$HOME/.local/share/icons/hicolor/scalable/apps"
desktop_dir="$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")"

mkdir -p "$bin_dir" "$app_dir" "$icon_dir" "$desktop_dir"

bin_path="$bin_dir/upulse"
echo "==> Installing binary  -> $bin_path"
install -m 0755 "$here/target/release/upulse" "$bin_path"

echo "==> Installing icon    -> $icon_dir/upulse.svg"
install -m 0644 "$here/assets/upulse.svg" "$icon_dir/upulse.svg"

# Write the launcher with an ABSOLUTE Exec so a desktop double-click always works
# (the desktop file manager may not have ~/.local/bin on its PATH). $2 is the
# Icon value: the app-menu copy uses the theme name, but the Desktop copy needs
# the absolute SVG path — the desktop-icons extension doesn't reliably resolve
# theme icons from ~/.local/share/icons.
write_desktop() {
  cat > "$1" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=UPulse
GenericName=System Control Center
Comment=Performance, storage, apps, system info and updates for your Ubuntu system
Exec=$bin_path
Icon=$2
Terminal=false
Categories=System;Monitor;
Keywords=system;monitor;cpu;memory;ram;disk;process;apps;packages;install;update;
StartupNotify=true
StartupWMClass=upulse
EOF
  chmod 0755 "$1"
}

echo "==> Installing launcher-> $app_dir/upulse.desktop"
write_desktop "$app_dir/upulse.desktop" "upulse"

echo "==> Creating desktop icon -> $desktop_dir/upulse.desktop"
write_desktop "$desktop_dir/upulse.desktop" "$icon_dir/upulse.svg"

# Mark the desktop launcher trusted so GNOME shows the icon instead of a
# "untrusted / allow launching?" prompt on first double-click.
if command -v gio >/dev/null 2>&1; then
  gio set "$desktop_dir/upulse.desktop" metadata::trusted true 2>/dev/null || true
fi

# Refresh the desktop + icon caches so everything appears immediately.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$app_dir" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true

echo
echo "Done!"
echo "  • Double-click 'UPulse' on your Desktop, or"
echo "  • find it in the app menu, or"
echo "  • run: upulse   (if ~/.local/bin is on your PATH)"
