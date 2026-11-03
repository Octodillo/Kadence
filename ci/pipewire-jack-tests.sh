#!/bin/sh
# Runs the JACK integration tests (daw-backend's, and daw-session's record
# test) on PipeWire's libjack (pipewire-jack), the way a Plasma desktop runs
# them, on a private headless PipeWire with a null sink and source standing
# in for a sound card.
#
# Needs: pipewire, pipewire-jack (pw-jack), wireplumber, dbus, cargo-nextest.
# No JACK example tools (jack_lsp): newer jackd2 packages no longer ship them.
# Extra arguments go to `cargo nextest run`.
set -eu

runtime=$(mktemp -d)
chmod 700 "$runtime"
export XDG_RUNTIME_DIR="$runtime"
pids=""

# Waits up to ten seconds for the command in "$@" to succeed, or fails with
# "$what".
wait_for() {
    i=0
    until "$@"; do
        i=$((i + 1))
        [ "$i" -lt 100 ] || { echo "$what" >&2; exit 1; }
        sleep 0.1
    done
}

cleanup() {
    # shellcheck disable=SC2086 # a list of pids
    [ -n "$pids" ] && kill $pids 2>/dev/null || true
    rm -rf "$runtime"
}
trap cleanup EXIT
# Exit on a signal, so the EXIT trap cleans up once and nothing runs after.
trap 'exit 130' INT
trap 'exit 143' TERM

dbus-daemon --session --address="unix:path=$runtime/bus" --nofork --nopidfile &
pids="$pids $!"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/bus"
what="the session bus did not start" wait_for test -S "$runtime/bus"
pipewire &
pids="$pids $!"
what="PipeWire did not start" wait_for test -S "$runtime/pipewire-0"
wireplumber &
pids="$pids $!"

for kind in Sink Source; do
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink
        node.name=ci-null-$kind media.class=Audio/$kind
        audio.position=[FL FR] object.linger=true }" >/dev/null
done

command -v pw-jack >/dev/null || { echo "pw-jack not found: install pipewire-jack" >&2; exit 1; }
# The fake sound card's ports are up once PipeWire lists them.
null_ports() {
    pw-link --output 2>/dev/null | grep -q '^ci-null-Source:'
}
what="PipeWire never showed the null source's ports" wait_for null_ports
pw-link --output --input

# One test at a time: pipewire-jack 1.0 (Ubuntu 24.04) can crash a client
# that stops and starts while other clients relink it in parallel test
# processes ("invalid mem id", then SIGSEGV), a race in the library, not in
# our code. The auto-connect test needs a dummy server's physical ports, so
# it is left out here rather than reported as passed.
pw-jack cargo nextest run -p daw-backend -p daw-session --run-ignored only --test-threads 1 \
    -E 'not test(auto_connect_reaches_physical_ports)' "$@"
