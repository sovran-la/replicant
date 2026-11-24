# Shared by the installer tests. Sourced, not run.
#
# Every test runs its installer in a sandbox: a temp HOME, a temp cargo target
# directory, and a PATH holding only stand-ins plus the handful of real tools
# the installers use. No test can reach the real cargo, systemctl, launchctl,
# codesign or security, or touch the real home directory.

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FAILS=0
TESTS=0
T=""

# sandbox: a fresh world for one test. Sets T, HOME, CALLS and the stand-in
# cargo; the narrowed PATH is applied by `run`, to the installer alone.
sandbox() {
    [ -z "$T" ] || rm -rf "$T"
    T="$(mktemp -d "${TMPDIR:-/tmp}/replicant-install-test.XXXXXX")"
    HOME="$T/home"
    CALLS="$T/calls"
    CARGO_TARGET_DIR="$T/target"
    CARGO="$T/bin/cargo"
    mkdir -p "$HOME" "$T/bin" "$T/tools"
    : >"$CALLS"
    for tool in cat cp mv rm mkdir dirname sed head id chmod; do
        ln -s "$(command -v "$tool")" "$T/tools/$tool"
    done
    export HOME CALLS CARGO CARGO_TARGET_DIR
    unset DEST SIGN_IDENTITY BUILD_FAILS BINARY_FAILS BINARY_SAYS
    stub_cargo
}

# stub NAME BODY: a stand-in command that records its arguments, then runs BODY.
stub() {
    {
        echo '#!/bin/sh'
        echo "echo \"$1 \$*\" >>\"\$CALLS\""
        echo "$2"
    } >"$T/bin/$1"
    chmod +x "$T/bin/$1"
}

# cargo's stand-in "builds" a replicant that answers --version. BUILD_FAILS
# makes the build fail; BINARY_FAILS makes the built binary unable to start;
# BINARY_SAYS changes what it prints, to tell one build from the next.
stub_cargo() {
    stub cargo '
[ -z "${BUILD_FAILS-}" ] || exit 101
mkdir -p "$CARGO_TARGET_DIR/release"
{
    echo "#!/bin/sh"
    if [ -n "${BINARY_FAILS-}" ]; then echo "exit 1"; fi
    echo "echo \"${BINARY_SAYS:-replicant 9.9.9}\""
} >"$CARGO_TARGET_DIR/release/replicant"
chmod +x "$CARGO_TARGET_DIR/release/replicant"'
}

# run SCRIPT: runs an installer on the sandbox's PATH, capturing its output
# and exit status. TEST_SH picks the shell (dash, say) to run it under.
run() {
    RC=0
    OUT="$(PATH="$T/bin:$T/tools" "${TEST_SH:-/bin/sh}" "$ROOT/scripts/$1" 2>&1)" || RC=$?
}

t() {
    TESTS=$((TESTS + 1))
    NAME="$1"
    sandbox
}

fail() {
    FAILS=$((FAILS + 1))
    echo "FAIL $NAME: $1"
    if [ -n "${OUT-}" ]; then echo "$OUT" | sed 's/^/    | /'; fi
}

eq() { # eq WHAT WANT GOT
    [ "$2" = "$3" ] || fail "$1: wanted '$2', got '$3'"
}

has() { # has WHAT NEEDLE HAYSTACK
    case "$3" in
        *"$2"*) ;;
        *) fail "$1: no '$2' in '$3'" ;;
    esac
}

lacks() { # lacks WHAT NEEDLE HAYSTACK
    case "$3" in
        *"$2"*) fail "$1: found '$2'" ;;
        *) ;;
    esac
}

exists() { [ -e "$1" ] || fail "missing $1"; }
absent() { [ ! -e "$1" ] || fail "should not exist: $1"; }

# The recorded calls of one stand-in, one per line.
calls() { grep "^$1 " "$CALLS" || true; }

finish() {
    [ -z "$T" ] || rm -rf "$T"
    if [ "$FAILS" -ne 0 ]; then
        echo "$FAILS of $TESTS failed"
        exit 1
    fi
    echo "ok: $TESTS tests"
}
