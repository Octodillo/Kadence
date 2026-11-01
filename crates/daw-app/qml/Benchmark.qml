// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Drives `--benchmark`: loads a 200 tracks x 50 clips project, scrolls and
// zooms the arrange view from code (no input events), prints frame times
// per phase, deletes the project and quits. Every frame moves the view, so
// the time between swapped frames is what one redraw costs. With `lanes`
// (`--benchmark-lanes`), every track shows volume and pan automation.
// Results go to standard output, whatever Qt's logging is set to. Each
// phase also gives the arrange view's own refresh and paint time per frame
// (median/p95/max; paint without the texture upload) and refreshes per
// frame, to tell its cost from the rest of the frame.
Item {
    id: bench

    required property Kirigami.ApplicationWindow window
    property bool lanes: false

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
    // The arrange view's own share of each frame: building its picture
    // (refresh) and painting it, in ms, and how many refreshes there were.
    property var refreshes: []
    property var paints: []
    property int refreshCount: 0
    // The timeline's size when measuring began.
    property string timeline: ""
    property int tick: 0
    property bool done: false

    function view(): Item {
        return window.pageStack.currentItem.view;
    }

    function say(...parts): void {
        TestHelper.printLine(["benchmark:"].concat(parts).join(" "), false);
    }

    function fail(message: string): void {
        TestHelper.printLine("benchmark: " + message, true);
    }

    function percentile(sorted: var, p: real): real {
        return sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
    }

    function report(): void {
        const sorted = frames.slice().sort((a, b) => a - b);
        if (sorted.length === 0) {
            say(phases[phase], "no frames");
            return;
        }
        const over = limit => sorted.filter(t => t > limit).length;
        say(phases[phase],
            "timeline", timeline, "px",
            "frames", sorted.length,
            "median", percentile(sorted, 0.5).toFixed(2),
            "p95", percentile(sorted, 0.95).toFixed(2),
            "p99", percentile(sorted, 0.99).toFixed(2),
            "max", sorted[sorted.length - 1].toFixed(2),
            "over16.7", over(16.7),
            "over33", over(33.4),
            "ms;", "arrange refresh", spread(refreshes),
            "paint", spread(paints), "ms,",
            (refreshCount / sorted.length).toFixed(2), "refreshes per frame");
    }

    // "median/p95/max" of `values`.
    function spread(values: var): string {
        const sorted = values.slice().sort((a, b) => a - b);
        return [percentile(sorted, 0.5), percentile(sorted, 0.95), sorted[sorted.length - 1]]
            .map(v => v.toFixed(2)).join("/");
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
        refreshes = [];
        paints = [];
        refreshCount = 0;
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
            // Since the last swap: the move made then and this frame's paint.
            const costs = bench.view().takeCosts();
            if (now - bench.phaseStart > bench.warmupMs && bench.last > 0) {
                if (bench.frames.length === 0) {
                    const v = bench.view();
                    bench.timeline = Math.round(v.contentWidth) + "x" + Math.round(v.contentHeight);
                }
                bench.frames.push(now - bench.last);
                bench.refreshes.push(costs[0]);
                bench.refreshCount += costs[1];
                bench.paints.push(costs[2]);
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
            if (!TestHelper.loadStressProject(bench.tracks, bench.clips, bench.lanes)) {
                bench.fail("could not build the stress project");
                TestHelper.removeTempFiles();
                Qt.exit(1);
                return;
            }
            bench.say(bench.tracks, "tracks x", bench.clips, "clips,",
                      bench.lanes ? "2 lanes each," : "no lanes,",
                      "window", bench.window.width + "x" + bench.window.height, "px");
            bench.next();
            bench.move();
        }
    }

    Timer {
        // Never hang: a stuck render loop still ends the run.
        interval: 60000
        running: true
        onTriggered: {
            bench.fail("timed out");
            TestHelper.removeTempFiles();
            Qt.exit(1);
        }
    }
}
