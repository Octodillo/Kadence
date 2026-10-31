// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Drives `--benchmark`: loads a 200 tracks x 50 clips project, scrolls and
// zooms the arrange view from code (no input events), prints frame times
// per phase, deletes the project and quits. Every frame moves the view, so
// the time between swapped frames is what one redraw costs.
Item {
    id: bench

    required property Kirigami.ApplicationWindow window

    readonly property int tracks: 200
    readonly property int clips: 50
    readonly property real phaseMs: 6000
    // Settle before measuring, so loading and first-frame costs don't count.
    readonly property real warmupMs: 1000

    readonly property var phases: ["vertical scroll", "horizontal scroll", "zoom"]
    property int phase: -1
    property real phaseStart: 0
    property real last: 0
    property var frames: []
    property int tick: 0
    property bool done: false

    function view(): Item {
        return window.pageStack.currentItem.view;
    }

    function percentile(sorted: var, p: real): real {
        return sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
    }

    function report(): void {
        const sorted = frames.slice().sort((a, b) => a - b);
        if (sorted.length === 0) {
            console.error("benchmark:", phases[phase], "no frames");
            return;
        }
        const over = limit => sorted.filter(t => t > limit).length;
        console.info("benchmark:", phases[phase],
                     "frames", sorted.length,
                     "median", percentile(sorted, 0.5).toFixed(2),
                     "p95", percentile(sorted, 0.95).toFixed(2),
                     "p99", percentile(sorted, 0.99).toFixed(2),
                     "max", sorted[sorted.length - 1].toFixed(2),
                     "over16.7", over(16.7),
                     "over33", over(33.4),
                     "ms");
    }

    // One step of the current phase's motion.
    function move(): void {
        const v = view();
        tick += 1;
        if (phase === 0) {
            const range = Math.max(1, v.contentHeight - v.height);
            const y = (tick * 37) % (2 * range);
            v.scrollTo(v.scrollX, y < range ? y : 2 * range - y);
        } else if (phase === 1) {
            const range = Math.max(1, v.contentWidth - v.width);
            const x = (tick * 41) % (2 * range);
            v.scrollTo(x < range ? x : 2 * range - x, v.scrollY);
        } else {
            // In for a second, out for a second, around the middle.
            v.pinch(Math.floor(tick / 60) % 2 === 0 ? 1.02 : 1 / 1.02, v.width / 2);
        }
    }

    function next(): void {
        if (phase >= 0) {
            report();
        }
        phase += 1;
        frames = [];
        tick = 0;
        if (phase >= phases.length) {
            done = true;
            TestHelper.removeTempFiles();
            Qt.quit();
            return;
        }
        view().scrollTo(0, 0);
        phaseStart = TestHelper.nowMs();
        last = 0;
    }

    Connections {
        target: bench.window
        enabled: bench.phase >= 0 && !bench.done
        function onFrameSwapped(): void {
            const now = TestHelper.nowMs();
            if (now - bench.phaseStart > bench.warmupMs && bench.last > 0) {
                bench.frames.push(now - bench.last);
            }
            bench.last = now;
            if (now - bench.phaseStart > bench.warmupMs + bench.phaseMs) {
                bench.next();
            }
            if (!bench.done) {
                bench.move();
            }
        }
    }

    Timer {
        // Let the window show before loading.
        interval: 500
        running: true
        onTriggered: {
            if (!TestHelper.loadStressProject(bench.tracks, bench.clips)) {
                console.error("benchmark: could not build the stress project");
                TestHelper.removeTempFiles();
                Qt.exit(1);
                return;
            }
            console.info("benchmark:", bench.tracks, "tracks x", bench.clips, "clips,",
                         bench.window.width + "x" + bench.window.height, "px");
            bench.next();
            bench.move();
        }
    }

    Timer {
        // Never hang: a stuck render loop still ends the run.
        interval: 60000
        running: true
        onTriggered: {
            console.error("benchmark: timed out");
            TestHelper.removeTempFiles();
            Qt.exit(1);
        }
    }
}
