#!/bin/sh
# Build and install replicant on this Mac, signing it when a signing identity
# is at hand, then run its daemon as a launchd job.
#
# The signature is what keeps macOS Local Network privacy and Little Snitch
# from treating every rebuild as a new program: both key on code identity,
# and an unsigned binary's identity is its hash. The identifier is fixed and
# the certificate is any "Apple Development" one in the login keychain
# (SIGN_IDENTITY overrides; SIGN_IDENTITY= skips signing). A Mac with no such
# certificate installs unsigned: the daemon works, and macOS asks about the
# local network again after each rebuild. Not for Linux hosts: see
# install-linux.sh.
#
# Safe to run again: the binary is replaced by rename, an existing launchd
# plist is left exactly as it is, and the job is restarted so it runs the
# binary just installed.
#
# Each step is its own statement so `set -e` stops on the first failure:
# errexit ignores a failing member of an `a && b` list.
set -eu
cd "$(dirname "$0")/.."

TARGET="${CARGO_TARGET_DIR:-target}"
"${CARGO:-$HOME/.cargo/bin/cargo}" build --release

# An unset SIGN_IDENTITY looks one up; a set one, even empty, is taken as is.
if [ -n "${SIGN_IDENTITY+set}" ]; then
    IDENT="$SIGN_IDENTITY"
else
    IDENT="$(security find-identity -v -p codesigning \
        | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)"
fi
if [ -n "$IDENT" ]; then
    codesign --force --sign "$IDENT" --identifier la.sovran.replicant --timestamp=none \
        "$TARGET/release/replicant"
    codesign --verify --strict "$TARGET/release/replicant"
else
    echo "no Apple Development identity; installing unsigned"
fi

DEST="${DEST:-$HOME/.cargo/bin/replicant}"
# Stage beside the destination, prove the staged binary runs, then rename:
# the running daemon keeps its old inode, and a binary that cannot start is
# never installed.
# A staged binary that fails its check is removed, not left beside $DEST.
trap 'rm -f "$DEST.new"' EXIT
mkdir -p "$(dirname "$DEST")"
cp "$TARGET/release/replicant" "$DEST.new"
"$DEST.new" --version
mv -f "$DEST.new" "$DEST"
echo "installed $DEST"

# The launchd job runs ~/.cargo/bin/replicant; restarting it after an install
# elsewhere would only restart the old binary.
if [ "$DEST" != "$HOME/.cargo/bin/replicant" ]; then
    echo "DEST is not the launchd job's binary; daemon not restarted"
    exit 0
fi

# The daemon's state directory: its logs go there, so it must exist before
# launchd starts the job.
STATE="$HOME/.sovran/replicant"
mkdir -p "$STATE"

NAME="la.sovran.replicant"
AGENT="$HOME/Library/LaunchAgents/$NAME.plist"
# Where the plist lived before this script wrote one: still honoured, so a
# machine set up by hand keeps the job it has.
LEGACY="$STATE/$NAME.plist"
if [ -f "$AGENT" ]; then
    PLIST="$AGENT"
    echo "keeping $PLIST"
elif [ -f "$LEGACY" ]; then
    PLIST="$LEGACY"
    echo "keeping $PLIST"
else
    PLIST="$AGENT"
    mkdir -p "$(dirname "$PLIST")"
    # Written whole to a sibling and renamed, so a failed write never leaves
    # half a plist for launchd to read.
    cat >"$PLIST.new" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$NAME</string>
  <key>ProgramArguments</key>
  <array><string>$HOME/.cargo/bin/replicant</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string></dict>
  <key>StandardOutPath</key><string>$STATE/daemon.out.log</string>
  <key>StandardErrorPath</key><string>$STATE/daemon.err.log</string>
</dict>
</plist>
EOF
    mv -f "$PLIST.new" "$PLIST"
    echo "wrote $PLIST"
fi

# `launchctl load` can print "Load failed" and still exit 0, so the job is
# looked up afterwards rather than trusting either command's status.
LABEL="gui/$(id -u)/$NAME"
if ! launchctl kickstart -k "$LABEL"; then
    launchctl unload "$PLIST" 2>/dev/null || true
    launchctl load "$PLIST"
fi
if ! launchctl print "$LABEL" >/dev/null 2>&1; then
    echo "the daemon is not loaded after the restart; check $PLIST" >&2
    exit 1
fi
echo "daemon restarted"
