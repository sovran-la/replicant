#!/bin/sh
# install-macos.sh, against stand-ins for cargo, security, codesign and
# launchctl.
. "$(dirname "$0")/lib.sh"

BIN_REL=".cargo/bin/replicant"
AGENT_REL="Library/LaunchAgents/la.sovran.replicant.plist"
LEGACY_REL=".sovran/replicant/la.sovran.replicant.plist"
IDENTITY='Apple Development: Someone (ABCDE12345)'

# security lists FOUND_IDENTITY, if any, the way find-identity prints it.
# launchctl: kickstart exits KICKSTART_RC, print exits PRINT_RC.
stub_mac() {
    stub security '
[ -z "${FOUND_IDENTITY-}" ] || echo "  1) 0123456789ABCDEF \"$FOUND_IDENTITY\""
echo "     1 valid identities found"'
    stub codesign '[ -z "${CODESIGN_FAILS-}" ] || exit 1'
    stub launchctl '
case "$1" in
    kickstart) exit "${KICKSTART_RC:-0}" ;;
    print) exit "${PRINT_RC:-0}" ;;
esac'
    unset FOUND_IDENTITY CODESIGN_FAILS KICKSTART_RC PRINT_RC
}

t "an identity in the keychain signs the binary"
stub_mac
FOUND_IDENTITY="$IDENTITY"
export FOUND_IDENTITY
run install-macos.sh
eq "exit" 0 "$RC"
has "signs with that identity" "--sign $IDENTITY" "$(calls codesign)"
has "under the fixed identifier" "--identifier la.sovran.replicant" "$(calls codesign)"
has "then verifies" "codesign --verify --strict" "$(calls codesign)"
exists "$HOME/$BIN_REL"
lacks "does not say unsigned" "unsigned" "$OUT"

t "no identity installs unsigned"
stub_mac
run install-macos.sh
eq "exit" 0 "$RC"
eq "codesign is never run" "" "$(calls codesign)"
has "says so" "installing unsigned" "$OUT"
exists "$HOME/$BIN_REL"
eq "the installed binary runs" "replicant 9.9.9" "$("$HOME/$BIN_REL" --version)"
has "the daemon is still started" "daemon restarted" "$OUT"

t "SIGN_IDENTITY is used without asking the keychain"
stub_mac
SIGN_IDENTITY="Developer ID Application: Other (ZZZZZ99999)"
export SIGN_IDENTITY
run install-macos.sh
eq "exit" 0 "$RC"
has "signs with it" "--sign Developer ID Application: Other (ZZZZZ99999)" "$(calls codesign)"
eq "the keychain is not consulted" "" "$(calls security)"

t "an empty SIGN_IDENTITY skips signing even when the keychain has one"
stub_mac
FOUND_IDENTITY="$IDENTITY"
SIGN_IDENTITY=""
export FOUND_IDENTITY SIGN_IDENTITY
run install-macos.sh
eq "exit" 0 "$RC"
eq "codesign is never run" "" "$(calls codesign)"
eq "the keychain is not consulted" "" "$(calls security)"
has "says so" "installing unsigned" "$OUT"

t "a failed signature installs nothing"
stub_mac
FOUND_IDENTITY="$IDENTITY"
CODESIGN_FAILS=1
export FOUND_IDENTITY CODESIGN_FAILS
run install-macos.sh
[ "$RC" -ne 0 ] || fail "should have failed"
absent "$HOME/$BIN_REL"
absent "$HOME/$AGENT_REL"
eq "launchd is not touched" "" "$(calls launchctl)"

t "a fresh Mac gets a launchd job in LaunchAgents"
stub_mac
run install-macos.sh
eq "exit" 0 "$RC"
exists "$HOME/$AGENT_REL"
absent "$HOME/$AGENT_REL.new"
absent "$HOME/$LEGACY_REL"
exists "$HOME/.sovran/replicant"
PL="$(cat "$HOME/$AGENT_REL")"
has "label" "<key>Label</key><string>la.sovran.replicant</string>" "$PL"
has "runs the installed binary as the daemon" \
    "<string>$HOME/$BIN_REL</string><string>daemon</string>" "$PL"
has "starts at login" "<key>RunAtLoad</key><true/>" "$PL"
has "logs beside the state" "<string>$HOME/.sovran/replicant/daemon.err.log</string>" "$PL"
has "says it wrote it" "wrote $HOME/$AGENT_REL" "$OUT"
has "restarts the job by label" "launchctl kickstart -k gui/$(id -u)/la.sovran.replicant" "$(calls launchctl)"
lacks "a job that kickstarts is not loaded again" "launchctl load" "$(calls launchctl)"

t "a plist already in LaunchAgents is kept byte for byte"
stub_mac
mkdir -p "$HOME/Library/LaunchAgents"
printf '<plist><!-- mine --></plist>\n' >"$HOME/$AGENT_REL"
cp "$HOME/$AGENT_REL" "$T/plist.before"
run install-macos.sh
eq "exit" 0 "$RC"
eq "plist unchanged" "$(cat "$T/plist.before")" "$(cat "$HOME/$AGENT_REL")"
has "says it kept it" "keeping $HOME/$AGENT_REL" "$OUT"

t "a hand-made plist in the state directory is kept and used"
stub_mac
mkdir -p "$HOME/.sovran/replicant"
printf '<plist><!-- by hand --></plist>\n' >"$HOME/$LEGACY_REL"
cp "$HOME/$LEGACY_REL" "$T/plist.before"
KICKSTART_RC=3
export KICKSTART_RC
run install-macos.sh
eq "exit" 0 "$RC"
eq "plist unchanged" "$(cat "$T/plist.before")" "$(cat "$HOME/$LEGACY_REL")"
absent "$HOME/$AGENT_REL"
has "loads that plist" "launchctl load $HOME/$LEGACY_REL" "$(calls launchctl)"

t "a job that is not loaded yet is loaded"
stub_mac
KICKSTART_RC=113
export KICKSTART_RC
run install-macos.sh
eq "exit" 0 "$RC"
eq "kickstart, then unload and load, then the lookup" \
    "launchctl kickstart -k gui/$(id -u)/la.sovran.replicant
launchctl unload $HOME/$AGENT_REL
launchctl load $HOME/$AGENT_REL
launchctl print gui/$(id -u)/la.sovran.replicant" \
    "$(calls launchctl)"

t "a job that is not there after the restart is an error"
stub_mac
PRINT_RC=113
export PRINT_RC
run install-macos.sh
eq "exit" 1 "$RC"
exists "$HOME/$BIN_REL"
has "names the plist" "check $HOME/$AGENT_REL" "$OUT"
lacks "does not claim success" "daemon restarted" "$OUT"

t "a second run replaces the binary and leaves the plist alone"
stub_mac
run install-macos.sh
cp "$HOME/$AGENT_REL" "$T/plist.first"
BINARY_SAYS="replicant 9.9.10"
export BINARY_SAYS
run install-macos.sh
eq "exit" 0 "$RC"
eq "the newer binary is installed" "replicant 9.9.10" "$("$HOME/$BIN_REL" --version)"
eq "plist unchanged" "$(cat "$T/plist.first")" "$(cat "$HOME/$AGENT_REL")"
absent "$HOME/$BIN_REL.new"

t "a binary that cannot start is never installed"
stub_mac
mkdir -p "$HOME/.cargo/bin"
printf '#!/bin/sh\necho old\n' >"$HOME/$BIN_REL"
chmod +x "$HOME/$BIN_REL"
BINARY_FAILS=1
export BINARY_FAILS
run install-macos.sh
[ "$RC" -ne 0 ] || fail "should have failed"
eq "the old binary is still there" "old" "$("$HOME/$BIN_REL")"
absent "$HOME/$BIN_REL.new"
absent "$HOME/$AGENT_REL"
eq "launchd is not touched" "" "$(calls launchctl)"

t "a failed build installs nothing"
stub_mac
BUILD_FAILS=1
export BUILD_FAILS
run install-macos.sh
[ "$RC" -ne 0 ] || fail "should have failed"
absent "$HOME/$BIN_REL"
absent "$HOME/$AGENT_REL"
eq "nothing is signed" "" "$(calls codesign)"

t "DEST elsewhere installs there and leaves launchd alone"
stub_mac
DEST="$T/elsewhere/replicant"
export DEST
run install-macos.sh
eq "exit" 0 "$RC"
exists "$T/elsewhere/replicant"
absent "$HOME/$BIN_REL"
absent "$HOME/$AGENT_REL"
eq "launchd is not touched" "" "$(calls launchctl)"
has "says why" "DEST is not the launchd job's binary" "$OUT"

finish
