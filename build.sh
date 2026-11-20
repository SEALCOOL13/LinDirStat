#!/usr/bin/env bash
# One-shot build script for Linux Mint / Ubuntu / Debian.
# Usage:  ./build.sh            (build only)
#         ./build.sh install    (build + install to ~/.local with a menu entry)
set -euo pipefail
cd "$(dirname "$0")"

echo "==> Installing build prerequisites (needs sudo)"
sudo apt-get update
sudo apt-get install -y build-essential curl pkg-config \
    libgl1 libegl1 libxkbcommon0 libxkbcommon-x11-0 libx11-6 libxcursor1 libxrandr2 libxi6 libwayland-client0

# The GUI toolkit needs Rust >= 1.88. Distro packages are usually older, so use rustup.
if ! command -v cargo >/dev/null 2>&1 || ! cargo --version | grep -Eq 'cargo 1\.(8[8-9]|9[0-9]|[0-9]{3})'; then
    echo "==> Installing Rust via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

echo "==> Building (first build downloads and compiles ~300 crates, a few minutes)"
cargo build --release

BIN="$PWD/target/release/windirstat-linux"
echo "==> Done: $BIN"

if [ "${1:-}" = "install" ]; then
    install -Dm755 "$BIN" "$HOME/.local/bin/windirstat-linux"
    install -Dm644 windirstat-linux.desktop "$HOME/.local/share/applications/windirstat-linux.desktop"
    echo "==> Installed to ~/.local/bin/windirstat-linux (menu entry: 'WinDirStat for Linux')"
fi
