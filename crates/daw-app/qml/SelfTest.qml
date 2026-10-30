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

    EngineErrorText {
        id: engineError
    }

    EditNames {
        id: editNames
    }

    ProjectMessageText {
        id: projectMessage
    }

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
            console.info("startup message:", engineError.active ? engineError.text : "none");
        },
        () => {
            check(window.visible, "window visible");
            check(window.pageStack.currentItem.title === i18nc("@title", "Arrange"), "starts on Arrange");
            shoot(window.title, "arrange");
        },
        () => {
            const arrange = window.pageStack.currentItem;
            check(Project.trackCount === 0, "new project is empty");
            arrange.addTrack(1);
            check(Project.trackCount === 1, "add instrument track");
            check(Project.trackName(0) === i18nc("@item default track name, %1 is a number", "Instrument %1", 1),
                  "default track name worded in QML");
            // Double-click the first lane: a one-bar MIDI clip appears.
            const v = arrange.view;
            v.doubleClick(v.width / 4, v.rulerHeight + v.trackHeight / 2, 0);
            check(Project.canUndo && Project.undoText !== "", "clip added");
            check(Project.undoText in editNames.names, "undo name has a translation");
            Project.undo();
            Project.undo();
            check(Project.trackCount === 0, "undo back to empty");
            // A failed import says why, worded in QML (checked next step).
            check(!v.dropFile("file:///nonexistent/take.wav", 10, 10), "missing file refused");
            const open = Project.openProject("file:///nonexistent/song.kadence");
            check(open.context === Project.OpenContext && open.reason === Project.FileAccess,
                  "open failure reported as data");
            console.info("open message:", projectMessage.text(open));
            check(Project.save().reason === Project.NoLocation, "save without a location refused");
        },
        () => {
            // Pick up the state now: on a slow first start the step timer can
            // fire before Project's own timer has ticked.
            Project.tick();
            check(Project.status.context === Project.ImportContext
                  && Project.status.reason === Project.CannotDecode
                  && Project.status.fileName === "take.wav", "import failure reported as data");
            console.info("import message:", projectMessage.text(Project.status));
            Project.clearStatus();
            check(projectMessage.text(Project.status) === "", "status cleared");
        },
        () => {
            check(TestHelper.loadDemoProject(), "load demo project");
            // Building the demo can outlast the 33 ms tick, and then this
            // timer may fire before it: refresh Project's properties now.
            Project.tick();
        },
        () => {
            // A slow load can block the event loop past this step's timer,
            // so both timers are overdue at once; refresh before reading.
            Project.tick();
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
        },
        () => {
            // The mixer has caught up with the project on its clock.
            // The demo's three tracks and the master.
            check(Mixer.levels.length === 4 * 5, "a strip per track");
            const compressor = Mixer.effectNames.indexOf("Compressor");
            check(compressor >= 0 && Mixer.addInsert(0, compressor), "add a compressor to Bass");
            // Play so the meters have something to show.
            window.playAction.trigger();
        },
        () => {
            check(Project.undoText === "Add Device", "mixer edits undo");
            check(Mixer.levels.some(v => v > 0), "meters move while playing");
            check(Mixer.levels[0 * 5 + 4] >= 0, "compressor gain reduction metered");
            Mixer.setSoloed(0, true);
            check(Mixer.anySoloed, "solo");
            Mixer.setSoloed(0, false);
            // Touch-write a fader move on Lead.
            Mixer.setWriting(true);
            Mixer.touch(1, "volume", -1, true);
            Mixer.setVolume(1, -12);
            shoot(window.title, "mix");
        },
        () => {
            Mixer.setVolume(1, -20);
            Mixer.touch(1, "volume", -1, false);
            Mixer.setWriting(false);
            window.playAction.trigger();
        },
        () => {
            check(Project.undoText === "Add Automation Lane", "fader move written as automation");
            check(!Mixer.writeAutomation, "writing off");
        },
        () => {
            // Click the compressor added above: its panel opens.
            const id = Mixer.insertDevice(0, 0);
            check(id !== "" && DevicePanel.open(id), "open the compressor's panel");
            check(DevicePanel.active && DevicePanel.deviceKind === "daw.compressor", "panel shows the compressor");
            check(DevicePanel.paramCount > 0, "compressor parameters listed");
            // Drag the threshold: one undo step.
            DevicePanel.setValue(0, -30, true);
            DevicePanel.setValue(0, -24, true);
            DevicePanel.endGesture();
            // Project's properties follow the document on its tick.
            Project.tick();
            check(Project.undoText === "Change Parameter", "parameter edit undoes");
            Project.undo();
            check(Project.undoText === "Add Automation Lane", "a drag is one undo step");
            Project.redo();
            DevicePanel.setDeviceEnabled(false);
            Project.tick();
            check(!DevicePanel.switchedOn && Project.undoText === "Bypass Device", "bypass");
            DevicePanel.setDeviceEnabled(true);
            check(DevicePanel.switchedOn, "switch back on");
        },
        () => {
            shoot(window.title, "mix-device");
        },
        () => {
            // The demo's Keys track plays the synth: its panel opens too.
            const keys = 2;
            check(DevicePanel.open(Mixer.instrumentDevice(keys)), "open the instrument's panel");
            check(DevicePanel.deviceKind === "daw.synth" && DevicePanel.instrument, "panel shows the synth");
            DevicePanel.close();
            check(!DevicePanel.active, "panel closed");
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
            check(Engine.applyAudioSettings(device, rate, size), "apply good audio settings");
            check(Engine.deviceIndex === device && Engine.sampleRate === rate && Engine.bufferSize === size,
                  "settings took effect");
            // Without an audio server the real engine reports AudioFailed;
            // only a settings error must be gone.
            check(Engine.errorContext === (Engine.running ? Engine.NoError : Engine.AudioFailed),
                  "only a failed engine reported after good settings");
            check(!Engine.applyAudioSettings(0, 12345, size), "bad sample rate refused");
            check(Engine.sampleRate === rate, "old settings kept after refusal");
            check(Engine.errorContext === Engine.ApplyingSettings
                  && Engine.errorKind === Engine.UnsupportedSampleRate
                  && Engine.errorNumber === 12345, "refusal reported as data");
            check(engineError.text === i18nc("@info %1 is the reason, a full sentence",
                                             "Could not change the audio settings. %1",
                                             i18nc("@info %1 is a number",
                                                   "The audio device does not support a sample rate of %1 Hz.",
                                                   (12345).toLocaleString(Qt.locale(), "f", 0))),
                  "refusal worded in QML");
            check(!Engine.applyAudioSettings(0, rate, 100), "bad buffer size refused");
            check(Engine.errorKind === Engine.UnsupportedBufferSize && Engine.errorNumber === 100
                  && engineError.reason === i18ncp("@info",
                                                   "The audio device does not support a buffer size of %1 sample.",
                                                   "The audio device does not support a buffer size of %1 samples.",
                                                   100),
                  "buffer size refusal worded with plural");
            check(!Engine.applyAudioSettings(Engine.deviceNames.length + 3, rate, size),
                  "unknown device index refused");
            check(Engine.errorKind === Engine.InvalidDeviceChoice
                  && Engine.errorNumber === Engine.deviceNames.length + 3, "bad device index reported");
            // Leave a known state in the config file for the test to read
            // (the stub offers 44100 and 512; a running server may not).
            const finalRate = Engine.sampleRates.includes(44100) ? 44100 : rate;
            const finalSize = Engine.bufferSizes.includes(512) ? 512 : size;
            check(Engine.applyAudioSettings(Math.min(1, device), finalRate, finalSize), "apply final settings");
            check(Engine.errorContext === (Engine.running ? Engine.NoError : Engine.AudioFailed),
                  "settings error cleared by good settings");
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
            window.openAbout();
            check(window.pageStack.layers.depth === 2, "About opened only once");
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
