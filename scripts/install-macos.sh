#!/bin/sh
# Build, sign and install replicant on this Mac, then restart its daemon.
#
# The signature is what keeps macOS Local Network privacy and Little Snitch
# from treating every rebuild as a new program: both key on code identity,
# and an unsigned binary's identity is its hash. The identifier is fixed and
# the certificate is any "Apple Development" one in the login keychain
# (SIGN_IDENTITY overrides). Not for Linux hosts: they install with cp.
#
# Each step is its own statement so `set -e` stops on the first failure:
# errexit ignores a failing member of an `a && b` list.
set -eu
cd "$(dirname "$0")/.."

IDENT="${SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)}"
if [ -z "$IDENT" ]; then
    echo "no Apple Development identity; set SIGN_IDENTITY" >&2
    exit 1
fi

"${CARGO:-$HOME/.cargo/bin/cargo}" build --release
codesign --force --sign "$IDENT" --identifier la.sovran.replicant --timestamp=none \
    target/release/replicant
codesign --verify --strict target/release/replicant

DEST="${DEST:-$HOME/.cargo/bin/replicant}"
# Stage beside the destination, prove the staged binary runs, then rename:
# the running daemon keeps its old inode, and a binary that cannot start is
# never installed.
cp target/release/replicant "$DEST.new"
"$DEST.new" --version
mv -f "$DEST.new" "$DEST"
echo "installed $DEST"

# The launchd job runs ~/.cargo/bin/replicant; restarting it after an install
# elsewhere would only restart the old binary.
if [ "$DEST" != "$HOME/.cargo/bin/replicant" ]; then
    echo "DEST is not the launchd job's binary; daemon not restarted"
    exit 0
fi
PLIST="$HOME/.sovran/replicant/la.sovran.replicant.plist"
if [ ! -f "$PLIST" ]; then
    echo "no $PLIST; no daemon to restart"
    exit 0
fi
if ! launchctl kickstart -k "gui/$(id -u)/la.sovran.replicant" 2>/dev/null; then
    launchctl unload "$PLIST" 2>/dev/null || true
    launchctl load "$PLIST"
fi
echo "daemon restarted"
