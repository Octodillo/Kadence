// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Drives the app for `--smoke-test` and `--screenshot-dir`, then quits with
// 0 on success. Each step runs after the previous one has had time to render.
Item {
    id: test

    required property Kirigami.ApplicationWindow window

    property int step: 0
    property Item settingsPage
    property int failures: 0

    function check(ok: bool, what: string): void {
        if (!ok) {
            console.error("smoke test: FAILED:", what);
            failures += 1;
        }
    }

    // Saves the window titled `title` as <screenshotDir>/<name>.png, if
    // screenshots were asked for.
    function shoot(title: string, name: string): void {
        if (window.screenshotDir !== "") {
            const path = window.screenshotDir + "/" + name + ".png";
            check(TestHelper.saveScreenshot(title, path), "save " + path);
        }
    }

    readonly property var steps: [
        () => {
            // What the engine started with, i.e. what was loaded from
            // kadencerc; the integration test checks this line.
            console.info("startup audio:", Engine.deviceIndex, Engine.sampleRate, Engine.bufferSize);
        },
        () => {
            check(window.visible, "window visible");
            check(window.pageStack.currentItem.title === i18nc("@title", "Arrange"), "starts on Arrange");
            shoot(window.title, "arrange");
        },
        () => {
            const arrange = window.pageStack.currentItem;
            check(Project.trackCount === 0, "new project is empty");
            Project.addTrack(1);
            check(Project.trackCount === 1, "add instrument track");
            // Double-click the first lane: a one-bar MIDI clip appears.
            const v = arrange.view;
            v.doubleClick(v.width / 4, v.rulerHeight + v.trackHeight / 2, 0);
            check(Project.canUndo && Project.undoText !== "", "clip added");
            Project.undo();
            Project.undo();
            check(Project.trackCount === 0, "undo back to empty");
        },
        () => {
            check(TestHelper.loadDemoProject(), "load demo project");
        },
        () => {
            check(Project.trackCount === 3, "demo has three tracks");
            // Peaks load in the background; give them a step, then shoot.
            window.pageStack.currentItem.view.refresh();
            shoot(window.title, "arrange-demo");
        },
        () => {
            Project.toggleLoop();
            check(Project.looping, "loop on");
            Project.togglePlay();
        },
        () => {
            check(Project.timeText !== "0:00.0", "playhead moves while playing");
            Project.togglePlay();
            Project.toggleLoop();
            shoot(window.title, "arrange-played");
        },
        () => {
            window.pageActions[1].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Mix"), "switch to Mix");
            check(Mixer.levels.length >= 10 && Mixer.effectNames.length > 0, "mixer has strips and effects");
            // Play so the meters have something to show.
            window.playAction.trigger();
        },
        () => {
            check(Mixer.levels.some(v => v > 0), "meters move while playing");
            const keys = 2;
            check(Mixer.levels[keys * 5 + 4] > 0, "compressor gain reduction metered");
            Mixer.setSoloed(0, true);
            check(Mixer.anySoloed, "solo");
            Mixer.setSoloed(0, false);
            shoot(window.title, "mix");
        },
        () => {
            window.playAction.trigger();
        },
        () => {
            window.pageActions[2].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Edit"), "switch to Edit");
            check(TestHelper.openFirstMidiClip(), "open the demo's MIDI clip");
            window.pageStack.currentItem.roll.refresh();
        },
        () => {
            const roll = window.pageStack.currentItem.roll;
            check(roll.hasClip, "piano roll has the clip");
            const notes = TestHelper.pianoNoteCount();
            check(notes === 12, "demo clip has 12 notes");
            // Double-click empty space low in the grid: a note is added.
            roll.doubleClick(roll.width * 0.8, roll.height - roll.velocityHeight - 20, 0);
            check(TestHelper.pianoNoteCount() === notes + 1, "double-click adds a note");
            Project.undo();
            check(TestHelper.pianoNoteCount() === notes, "undo removes it");
            roll.selectAll();
            check(roll.hasSelection, "select all");
            shoot(window.title, "edit");
        },
        () => {
            const keys = TestHelper.pianoKeySum();
            window.pageStack.currentItem.roll.transpose(1);
            check(TestHelper.pianoKeySum() === keys + TestHelper.pianoNoteCount(), "transpose up a semitone");
            Project.undo();
            check(TestHelper.pianoKeySum() === keys, "undo the transpose");
            window.pageActions[0].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Arrange"), "back to Arrange");
        },
        () => {
            window.playAction.trigger();
            check(Project.playing, "play");
            window.playAction.trigger();
            check(!Project.playing, "stop");
        },
        () => {
            settingsPage = window.openSettings();
            check(settingsPage !== null, "open settings");
        },
        () => {
            shoot(settingsPage.Window.window.title, "settings");
        },
        () => {
            if (!window.smokeTest) {
                return;
            }
            // Pick from what the engine offers: the stub has several devices,
            // the real engine (silent without a server) has one.
            const device = Engine.deviceNames.length - 1;
            const rate = Engine.sampleRates[Engine.sampleRates.length - 1];
            const size = Engine.bufferSizes[1];
            check(Engine.applyAudioSettings(device, rate, size) === "", "apply good audio settings");
            check(Engine.deviceIndex === device && Engine.sampleRate === rate && Engine.bufferSize === size,
                  "settings took effect");
            check(Engine.applyAudioSettings(0, 12345, size) !== "", "bad sample rate refused");
            check(Engine.sampleRate === rate, "old settings kept after refusal");
            check(Engine.errorText !== "", "error shown");
            // Leave a known state in the config file for the test to read
            // (the stub offers 44100 and 512; a running server may not).
            const finalRate = Engine.sampleRates.includes(44100) ? 44100 : rate;
            const finalSize = Engine.bufferSizes.includes(512) ? 512 : size;
            check(Engine.applyAudioSettings(Math.min(1, device), finalRate, finalSize) === "",
                  "apply final settings");
        },
        () => {
            // Older Kirigami (KF 6.13 in Debian 13) has no PageStack.closeDialog();
            // the dialog layer is its own window, so close that instead.
            const stack = settingsPage.Kirigami.PageStack;
            if (typeof stack.closeDialog === "function") {
                stack.closeDialog();
            } else {
                settingsPage.Window.window.close();
            }
            window.openAbout();
            check(window.pageStack.layers.depth === 2, "open About");
        },
        () => {
            shoot(window.title, "about");
        },
        () => {
            window.pageStack.layers.pop();
        }
    ]

    Timer {
        interval: 400
        repeat: true
        running: true
        onTriggered: {
            if (test.step < test.steps.length) {
                // Advance first so a step that throws can't repeat forever.
                const current = test.step;
                test.step += 1;
                try {
                    test.steps[current]();
                } catch (error) {
                    test.check(false, "step " + current + " threw: " + error);
                }
                return;
            }
            stop();
            console.info("smoke test:", test.failures === 0 ? "passed" : "FAILED");
            Qt.exit(test.failures === 0 ? 0 : 3);
        }
    }
}
