#!/bin/sh
# Run `kadence --smoke-test` from an installed package on every display
# setup a Plasma user might have: X11, Wayland, and the offscreen platform
# at 100%, fractional and 200% scaling.
#
#   ci/smoke-matrix.sh [path/to/kadence]
#
# CI machines have no GPU, so Qt Quick renders in software everywhere.
#
# X11 needs xvfb-run, Wayland needs weston; a missing one is skipped with a
# note unless REQUIRE_ALL=1.
set -eu

export QT_QUICK_BACKEND=software

kadence=${1:-kadence}
failed=0
weston_pid=
runtime=

cleanup() {
    if [ -n "$weston_pid" ]; then
        kill "$weston_pid" 2>/dev/null || true
        wait "$weston_pid" 2>/dev/null || true
    fi
    if [ -n "$runtime" ]; then
        rm -rf "$runtime"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

run() {
    name=$1
    shift
    config=$(mktemp -d)
    if env XDG_CONFIG_HOME="$config" QT_FORCE_STDERR_LOGGING=1 "$@" \
        timeout 120 "$kadence" --smoke-test >"$config/log" 2>&1 \
        && grep -q "smoke test: passed" "$config/log"; then
        echo "ok: $name"
    else
        echo "FAILED: $name"
        tail -n 40 "$config/log" | sed 's/^/    /'
        failed=1
    fi
    rm -rf "$config"
}

skip() {
    if [ "${REQUIRE_ALL:-0}" = 1 ]; then
        echo "FAILED: $1 ($2 not installed)"
        failed=1
    else
        echo "skipped: $1 ($2 not installed)"
    fi
}

for scale in 1 1.25 1.5 2; do
    run "offscreen, scale $scale" QT_QPA_PLATFORM=offscreen QT_SCALE_FACTOR=$scale
done

if command -v xvfb-run >/dev/null; then
    run "X11 (Xvfb)" QT_QPA_PLATFORM=xcb xvfb-run -a -s "-screen 0 1920x1080x24"
else
    skip "X11" xvfb-run
fi

if command -v weston >/dev/null; then
    runtime=$(mktemp -d)
    chmod 700 "$runtime"
    XDG_RUNTIME_DIR=$runtime weston --backend=headless --socket=kadence-smoke \
        --width=1920 --height=1080 >"$runtime/weston.log" 2>&1 &
    weston_pid=$!
    i=0
    while [ ! -S "$runtime/kadence-smoke" ] && [ $i -lt 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    if [ -S "$runtime/kadence-smoke" ]; then
        run "Wayland (weston headless)" XDG_RUNTIME_DIR="$runtime" \
            WAYLAND_DISPLAY=kadence-smoke QT_QPA_PLATFORM=wayland
        run "Wayland (weston headless), scale 1.25" XDG_RUNTIME_DIR="$runtime" \
            WAYLAND_DISPLAY=kadence-smoke QT_QPA_PLATFORM=wayland QT_SCALE_FACTOR=1.25
    else
        echo "FAILED: weston did not start"
        sed 's/^/    /' "$runtime/weston.log"
        failed=1
    fi
    cleanup
    weston_pid=
    runtime=
else
    skip "Wayland" weston
fi

exit $failed
