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
            window.pageActions[1].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Mix"), "switch to Mix");
            shoot(window.title, "mix");
        },
        () => {
            window.pageActions[2].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Edit"), "switch to Edit");
            window.pageActions[0].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Arrange"), "back to Arrange");
        },
        () => {
            window.playAction.trigger();
            check(Engine.playing, "play");
            window.playAction.trigger();
            check(!Engine.playing, "stop");
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
            check(Engine.applyAudioSettings(2, 96000, 128) === "", "apply good audio settings");
            check(Engine.deviceIndex === 2 && Engine.sampleRate === 96000 && Engine.bufferSize === 128,
                  "settings took effect");
            check(Engine.applyAudioSettings(0, 12345, 128) !== "", "bad sample rate refused");
            check(Engine.sampleRate === 96000, "old settings kept after refusal");
            check(Engine.errorText !== "", "error shown");
            // Leave a known state in the config file for the test to read.
            check(Engine.applyAudioSettings(1, 44100, 512) === "", "apply final settings");
        },
        () => {
            settingsPage.Kirigami.PageStack.closeDialog();
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
