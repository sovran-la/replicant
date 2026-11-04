#!/bin/sh
# Build, sign and install replicant on this Mac, then restart its daemon.
#
# The signature is what keeps macOS Local Network privacy and Little Snitch
# from treating every rebuild as a new program: both key on code identity,
# and an unsigned binary's identity is its hash. The identifier is fixed and
# the certificate is any "Apple Development" one in the login keychain
# (SIGN_IDENTITY overrides). Not for Linux hosts: they install with cp.
set -eu
cd "$(dirname "$0")/.."

IDENT="${SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)}"
[ -n "$IDENT" ] || { echo "no Apple Development identity; set SIGN_IDENTITY" >&2; exit 1; }

"${CARGO:-$HOME/.cargo/bin/cargo}" build --release
codesign --force --sign "$IDENT" --identifier la.sovran.replicant --timestamp=none target/release/replicant
codesign --verify --strict target/release/replicant

DEST="${DEST:-$HOME/.cargo/bin/replicant}"
# Replace by rename: the running daemon keeps its old inode.
cp target/release/replicant "$DEST.new" && mv -f "$DEST.new" "$DEST"
"$DEST" --version

PLIST="$HOME/.sovran/replicant/la.sovran.replicant.plist"
if [ -f "$PLIST" ]; then
    launchctl kickstart -k "gui/$(id -u)/la.sovran.replicant" 2>/dev/null \
        || { launchctl unload "$PLIST" 2>/dev/null || true; launchctl load "$PLIST"; }
    echo "daemon restarted"
fi
