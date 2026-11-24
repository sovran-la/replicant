#!/bin/sh
# install-linux.sh, against stand-ins for cargo and systemctl.
. "$(dirname "$0")/lib.sh"

BIN_REL=".cargo/bin/replicant"
UNIT_REL=".config/systemd/user/replicant.service"

# systemctl's stand-in: `is-active` answers with SYSTEMCTL_ACTIVE (0 = active).
stub_systemctl() {
    stub systemctl '
case "$*" in
    *is-active*) exit "${SYSTEMCTL_ACTIVE:-0}" ;;
esac'
}

t "a fresh machine gets the binary, the unit and a running service"
stub_systemctl
run install-linux.sh
eq "exit" 0 "$RC"
exists "$HOME/$BIN_REL"
eq "the installed binary runs" "replicant 9.9.9" "$("$HOME/$BIN_REL" --version)"
absent "$HOME/$BIN_REL.new"
exists "$HOME/$UNIT_REL"
absent "$HOME/$UNIT_REL.new"
exists "$HOME/.sovran/replicant"
has "unit runs the installed binary" "ExecStart=$HOME/$BIN_REL daemon" "$(cat "$HOME/$UNIT_REL")"
has "unit restarts on failure" "Restart=on-failure" "$(cat "$HOME/$UNIT_REL")"
has "unit starts with the session" "WantedBy=default.target" "$(cat "$HOME/$UNIT_REL")"
eq "systemctl calls, in order" \
    "systemctl --user daemon-reload
systemctl --user enable replicant.service
systemctl --user restart replicant.service
systemctl --user is-active --quiet replicant.service" \
    "$(calls systemctl)"
has "says where it installed" "installed $HOME/$BIN_REL" "$OUT"
has "says the daemon runs" "daemon running" "$OUT"

t "an existing unit is kept byte for byte"
stub_systemctl
mkdir -p "$HOME/.config/systemd/user"
printf '[Service]\nExecStart=/somewhere/else daemon\n# mine\n' >"$HOME/$UNIT_REL"
cp "$HOME/$UNIT_REL" "$T/unit.before"
run install-linux.sh
eq "exit" 0 "$RC"
eq "unit unchanged" "$(cat "$T/unit.before")" "$(cat "$HOME/$UNIT_REL")"
has "says it kept the unit" "keeping $HOME/$UNIT_REL" "$OUT"
has "still restarts the service" "systemctl --user restart replicant.service" "$(calls systemctl)"

t "a second run replaces the binary and leaves the unit alone"
stub_systemctl
run install-linux.sh
cp "$HOME/$UNIT_REL" "$T/unit.first"
BINARY_SAYS="replicant 9.9.10"
export BINARY_SAYS
run install-linux.sh
eq "exit" 0 "$RC"
eq "the newer binary is installed" "replicant 9.9.10" "$("$HOME/$BIN_REL" --version)"
eq "unit unchanged" "$(cat "$T/unit.first")" "$(cat "$HOME/$UNIT_REL")"
absent "$HOME/$BIN_REL.new"

t "a binary that cannot start is never installed"
stub_systemctl
mkdir -p "$HOME/.cargo/bin"
printf '#!/bin/sh\necho old\n' >"$HOME/$BIN_REL"
chmod +x "$HOME/$BIN_REL"
BINARY_FAILS=1
export BINARY_FAILS
run install-linux.sh
[ "$RC" -ne 0 ] || fail "should have failed"
eq "the old binary is still there" "old" "$("$HOME/$BIN_REL")"
absent "$HOME/$BIN_REL.new"
absent "$HOME/$UNIT_REL"
eq "the service is not touched" "" "$(calls systemctl)"

t "a failed build installs nothing"
stub_systemctl
BUILD_FAILS=1
export BUILD_FAILS
run install-linux.sh
[ "$RC" -ne 0 ] || fail "should have failed"
absent "$HOME/$BIN_REL"
absent "$HOME/$UNIT_REL"
eq "the service is not touched" "" "$(calls systemctl)"

t "DEST elsewhere installs there and leaves the service alone"
stub_systemctl
DEST="$T/elsewhere/replicant"
export DEST
run install-linux.sh
eq "exit" 0 "$RC"
exists "$T/elsewhere/replicant"
absent "$T/elsewhere/replicant.new"
absent "$HOME/$BIN_REL"
absent "$HOME/$UNIT_REL"
eq "the service is not touched" "" "$(calls systemctl)"
has "says why" "DEST is not the service's binary" "$OUT"

t "a service that does not come up is an error"
stub_systemctl
SYSTEMCTL_ACTIVE=3
export SYSTEMCTL_ACTIVE
run install-linux.sh
unset SYSTEMCTL_ACTIVE
eq "exit" 1 "$RC"
exists "$HOME/$BIN_REL"
has "points at the journal" "journalctl --user -u replicant" "$OUT"
lacks "does not claim success" "daemon running" "$OUT"

t "no systemctl is an error that says how to start the daemon"
run install-linux.sh
eq "exit" 1 "$RC"
exists "$HOME/$BIN_REL"
absent "$HOME/$UNIT_REL"
has "says what to run" "$HOME/$BIN_REL daemon" "$OUT"

t "a failing systemctl stops the install"
stub systemctl '
case "$*" in
    *enable*) exit 1 ;;
esac'
run install-linux.sh
[ "$RC" -ne 0 ] || fail "should have failed"
lacks "restart is not reached" "restart" "$(calls systemctl)"
lacks "does not claim success" "daemon running" "$OUT"

finish
