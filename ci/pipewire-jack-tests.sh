#!/bin/sh
# Runs the daw-backend JACK integration tests on PipeWire's libjack
# (pipewire-jack), the way a Plasma desktop runs them, on a private headless
# PipeWire with a null sink and source standing in for a sound card.
#
# Needs: pipewire, pipewire-jack (pw-jack), wireplumber, dbus, cargo-nextest.
# Extra arguments go to `cargo nextest run`.
set -eu

runtime=$(mktemp -d)
chmod 700 "$runtime"
export XDG_RUNTIME_DIR="$runtime"
pids=""
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
pipewire &
pids="$pids $!"
# Wait for the socket before starting the session manager.
i=0
while [ ! -S "$runtime/pipewire-0" ]; do
    i=$((i + 1))
    [ "$i" -lt 100 ] || { echo "PipeWire did not start" >&2; exit 1; }
    sleep 0.1
done
wireplumber &
pids="$pids $!"

for kind in Sink Source; do
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink
        node.name=ci-null-$kind media.class=Audio/$kind
        audio.position=[FL FR] object.linger=true }" >/dev/null
done

i=0
until pw-jack jack_lsp >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -lt 100 ] || { echo "pipewire-jack does not answer" >&2; exit 1; }
    sleep 0.1
done
pw-jack jack_lsp

# One test at a time: pipewire-jack 1.0 (Ubuntu 24.04) can crash a client
# that stops and starts while other clients relink it in parallel test
# processes ("invalid mem id", then SIGSEGV), a race in the library, not in
# our code. The auto-connect test needs a dummy server's physical ports, so
# it is left out here rather than reported as passed.
pw-jack cargo nextest run -p daw-backend --run-ignored only --test-threads 1 \
    -E 'not test(auto_connect_reaches_physical_ports)' "$@"
