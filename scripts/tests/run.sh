#!/bin/sh
# Runs every installer test. Each one sandboxes the installer it tests, so
# this is safe on any machine, Mac or Linux: sh scripts/tests/run.sh
set -u
cd "$(dirname "$0")" || exit 1
rc=0
for test in t-*.sh; do
    echo "== $test"
    sh "$test" || rc=1
done
exit "$rc"
