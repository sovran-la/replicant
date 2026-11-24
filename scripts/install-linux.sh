#!/bin/sh
# Build and install replicant on this Linux machine, then run its daemon as a
# systemd user service.
#
# Safe to run again: the binary is replaced by rename, an existing unit file is
# left exactly as it is, and the service is restarted so it runs the binary
# just installed.
#
# Each step is its own statement so `set -e` stops on the first failure:
# errexit ignores a failing member of an `a && b` list.
set -eu
cd "$(dirname "$0")/.."

TARGET="${CARGO_TARGET_DIR:-target}"
"${CARGO:-$HOME/.cargo/bin/cargo}" build --release

DEST="${DEST:-$HOME/.cargo/bin/replicant}"
# Stage beside the destination, prove the staged binary runs, then rename:
# the running daemon keeps its old inode (a plain cp over it fails with "Text
# file busy"), and a binary that cannot start is never installed.
# A staged binary that fails its check is removed, not left beside $DEST.
trap 'rm -f "$DEST.new"' EXIT
mkdir -p "$(dirname "$DEST")"
cp "$TARGET/release/replicant" "$DEST.new"
"$DEST.new" --version
mv -f "$DEST.new" "$DEST"
echo "installed $DEST"

# The unit runs ~/.cargo/bin/replicant; restarting it after an install
# elsewhere would only restart the old binary.
if [ "$DEST" != "$HOME/.cargo/bin/replicant" ]; then
    echo "DEST is not the service's binary; service not touched"
    exit 0
fi

if ! command -v systemctl >/dev/null 2>&1; then
    echo "no systemctl; start the daemon yourself: $DEST daemon" >&2
    exit 1
fi

# The daemon's state directory, so its first start has somewhere to write.
mkdir -p "$HOME/.sovran/replicant"

UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/replicant.service"
if [ -f "$UNIT" ]; then
    echo "keeping $UNIT"
else
    mkdir -p "$UNIT_DIR"
    # Written whole to a sibling and renamed, so a failed write never leaves
    # half a unit for systemd to read.
    cat >"$UNIT.new" <<EOF
[Unit]
Description=replicant daemon
After=network-online.target tailscaled.service

[Service]
ExecStart=$HOME/.cargo/bin/replicant daemon
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
EOF
    mv -f "$UNIT.new" "$UNIT"
    echo "wrote $UNIT"
fi

systemctl --user daemon-reload
systemctl --user enable replicant.service
# `restart` starts a stopped unit too, so one command covers a first install
# and an upgrade.
systemctl --user restart replicant.service
if ! systemctl --user is-active --quiet replicant.service; then
    echo "the daemon is not running after the restart; see: journalctl --user -u replicant" >&2
    exit 1
fi
echo "daemon running"
