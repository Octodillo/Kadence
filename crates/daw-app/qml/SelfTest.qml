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
    // Made by TestHelper.makeFilesTest().
    property string filesDir
    property var browser
    property var picked: null
    // Times steps have asked to be run again.
    property int waits: 0
    // What the export dialog reported last, and where it exported to.
    property var exportMessage: ({})
    property url exportUrl

    EngineErrorText {
        id: engineError
    }

    EditNames {
        id: editNames
    }

    ProjectMessageText {
        id: projectMessage
    }

    Connections {
        target: Exporter
        function onFinished(message: var): void {
            test.exportMessage = message;
        }
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
            // Dragging a header's lower edge resizes every track.
            const v = arrange.view;
            const close = (a, b) => Math.abs(a - b) < 0.5;
            const before = v.trackHeight;
            const target = before > 40 ? before - 10 : before + 10;
            v.resizeTracks(target, v.rulerHeight);
            check(close(v.trackHeight, target) && close(v.rowHeight(0), target), "resize tracks");
            v.resizeTracks(1, v.rulerHeight);
            check(v.trackHeight >= 24, "tracks keep a minimum height");
            v.resizeTracks(before, v.rulerHeight);
            check(close(v.trackHeight, before), "resize tracks back");
            // The edge follows the pointer: scrolled, the sizes above the
            // row scroll away; at the top, they share the movement.
            const header = arrange.trackHeaders.itemAtIndex(0);
            check(header.edgeChange(12, 100, 2, 1, 10, 2000, 500) === 12, "a dragged edge follows the pointer");
            check(header.edgeChange(12, 0, 2, 1, 3, 192, 500) === 4 && header.edgeChange(12, 0, 1, 2, 3, 192, 500) === 4,
                  "in a short timeline, the sizes above a dragged edge share the movement");
            check(header.edgeChange(-30, 10, 1, 1, 10, 2000, 500) === -20, "an edge scrolls to the top, then shares the rest");
            // Double-click the first lane: a one-bar MIDI clip appears.
            v.doubleClick(v.width / 4, v.rulerHeight + v.trackHeight / 2, 0);
            check(Project.canUndo && Project.undoText !== "", "clip added");
            check(Project.undoText in editNames.names, "undo name has a translation");
            // Arm it and pick a MIDI input: both undoable, both translated.
            Project.setTrackArmed(0, true);
            check(Project.trackArmed(0) && Project.undoText === "Arm Track", "arm a track");
            Project.setTrackMidiInput(0, "Midi-Bridge:USB Keyboard (capture)", 9);
            check(Project.trackMidiDevice(0) === "Midi-Bridge:USB Keyboard (capture)"
                  && Project.trackMidiChannel(0) === 9
                  && Project.undoText === "Change MIDI Input"
                  && Project.undoText in editNames.names, "choose a MIDI input");
            Project.undo();
            check(Project.trackMidiDevice(0) === "" && Project.trackMidiChannel(0) === -1, "undo the MIDI input");
            Project.undo();
            check(!Project.trackArmed(0), "undo arming");
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
            check(Project.trackCount === 4, "demo has three tracks and a bus");
            // Show an armed instrument track listening to one channel.
            for (let i = 0; i < Project.trackCount; i++) {
                if (Project.trackKind(i) === 1) {
                    Project.setTrackArmed(i, true);
                    Project.setTrackMidiInput(i, "", 0);
                    break;
                }
            }
            Project.tick();
            // Peaks load in the background; give them a step, then shoot.
            window.pageStack.currentItem.view.refresh();
            shoot(window.title, "arrange-demo");
        },
        () => {
            // Show the Bass and Keys lanes, then add a point to Bass's
            // volume lane with a click.
            const v = window.pageStack.currentItem.view;
            check(Automation.laneCount(0) === 1 && Automation.laneKind(0, 0) === 0, "demo Bass has a volume lane");
            check(Automation.laneMode(0, 0) === 1, "lanes read by default");
            Automation.setExpanded(0, true);
            Automation.setExpanded(2, true);
            check(Automation.expanded(0) && v.rowHeight(0) === v.trackHeight + v.laneHeight, "Bass shows its lane");
            // A lane header's lower edge resizes the lanes, within limits;
            // a double-click puts the default back.
            // The test leaves the user's saved sizes as they were.
            const lanes = window.pageStack.currentItem.laneSize;
            const savedLanes = lanes.units;
            lanes.begin(v.laneHeight, Infinity);
            lanes.drag(Kirigami.Units.gridUnit, false);
            check(Math.abs(v.laneHeight - Math.round(lanes.pixels)) < 1 && lanes.units > lanes.defaultUnits, "resize lanes");
            lanes.drag(-1000, false);
            check(lanes.units === lanes.minimumUnits, "lanes keep a minimum height");
            lanes.reset();
            check(lanes.units === lanes.defaultUnits && v.rowHeight(0) === v.trackHeight + v.laneHeight, "lane height reset");
            lanes.units = savedLanes;
            AppSettings.setLayoutValue(lanes.key, savedLanes);
            const y = v.rulerHeight + v.trackHeight + v.laneHeight / 2;
            v.press(v.width * 0.45, y, 1, 0);
            v.release(v.width * 0.45, y, 0);
            Project.tick();
            check(Project.undoText === "Edit Automation", "click adds an automation point");
            check(Project.undoText in editNames.names, "automation edit has a translation");
            check(Automation.targetCount(0) >= 1 && Automation.targetKind(0, 0) === 1, "Pan can get a lane");
            Automation.setLaneMode(0, 0, 0);
            Project.tick();
            check(Automation.laneMode(0, 0) === 0 && Project.undoText === "Toggle Automation", "lane off");
            Automation.setLaneMode(0, 0, 2);
            check(Automation.laneMode(0, 0) === 2, "lane in write mode");
            Automation.setLaneMode(0, 0, 1);
            Project.undo();
            Project.undo();
            Project.undo();
            v.refresh();
        },
        () => {
            shoot(window.title, "arrange-automation");
        },
        () => {
            Automation.setExpanded(0, false);
            Automation.setExpanded(2, false);
            check(!Automation.expanded(0), "lanes hidden");
        },
        () => {
            // Tempo and time signature: edits in a row are one undo step.
            check(Project.bpm === 120 && Project.meterNumerator === 4 && Project.meterDenominator === 4,
                  "demo starts at 120 bpm in 4/4");
            Project.setTempo(96);
            Project.setTempo(98.5);
            check(Project.bpm === 98.5, "tempo set");
            check(Project.undoText === "Change Tempo" && Project.undoText in editNames.names, "tempo edit undoes");
            Project.setTimeSignature(6, 7);
            check(Project.meterNumerator === 6 && Project.meterDenominator === 4, "time signature set, rounded");
            check(Project.undoText === "Change Time Signature" && Project.undoText in editNames.names,
                  "time signature edit undoes");
            Project.undo();
            check(Project.meterNumerator === 4 && Project.bpm === 98.5, "undo the time signature");
            Project.undo();
            check(Project.bpm === 120, "undo the tempo drag in one step");
            Project.redo();
            check(Project.bpm === 98.5, "redo");
            Project.setTimeSignature(3, 4);
            // The metronome, with its settings open, for a screenshot.
            Project.setMetronomeEnabled(true);
            Project.setCountIn(1);
            check(Project.metronome && Project.countInBars === 1, "metronome settings");
            const arrange = window.pageStack.currentItem;
            arrange.transportBar.openClickSettings();
        },
        () => {
            shoot(window.title, "arrange-metronome");
            window.pageStack.currentItem.transportBar.closeClickSettings();
            Project.setMetronomeEnabled(false);
            Project.setCountIn(0);
            Project.undo();
            Project.undo();
            check(Project.bpm === 120 && Project.meterNumerator === 4, "back to the demo's tempo");
        },
        () => {
            const dialog = window.openExport();
            check(dialog.opened || dialog.visible, "open the export dialog");
            check(Exporter.songBars === 12, "demo is twelve bars long");
        },
        () => {
            shoot(window.title, "export");
            // Export as the dialog's Export… button does once a file is
            // chosen, with the dialog's defaults (24-bit WAV). The file name
            // has no extension, so the format's is added.
            const dialog = window.openExport();
            exportUrl = TestHelper.tempFileUrl("demo-export");
            dialog.start(exportUrl);
            check(Exporter.running, "export started");
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
            // The mixer catches up with the project on its own clock, which
            // a loaded machine can take a few ticks for.
            // The demo's three tracks, its reverb bus and the master.
            if (Mixer.levels.length !== 5 * 5 && test.waits < 25) {
                return false;
            }
            test.waits = 0;
            check(Mixer.levels.length === 5 * 5, "a strip per track");
            const compressor = Mixer.effectNames.indexOf("Compressor");
            check(compressor >= 0 && Mixer.addInsert(0, compressor), "add a compressor to Bass");
            // Play so the meters have something to show.
            window.playAction.trigger();
        },
        () => {
            // The engine swaps in the new graph on its own time, and a busy
            // machine (CI runs several app instances at once) can take more
            // than one step to bring the compressor's meter up.
            const metered = Mixer.levels.some((v, i) => i % 5 < 4 && v > 0) && Mixer.levels[0 * 5 + 4] >= 0;
            if (!metered && test.waits < 25) {
                return false;
            }
            test.waits = 0;
            check(Project.undoText === "Add Device", "mixer edits undo");
            check(Mixer.levels.some((v, i) => i % 5 < 4 && v > 0), "meters move while playing");
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
            // Routing on Keys (row 2): a send to the reverb bus, tapped
            // before the fader, and the output through the bus and back.
            // Project picks up mixer edits on its clock; tick to read them.
            check(Mixer.addSend(2, 0), "add a send to Reverb");
            Project.tick();
            check(Project.undoText === "Add Send", "sends undo");
            Mixer.setSendPreFader(2, 0, true);
            Project.tick();
            check(Project.undoText === "Change Send Tap", "pre-fader send");
            Mixer.setOutput(2, 1);
            Project.tick();
            check(Project.undoText === "Change Track Output", "output to the bus");
            Project.undo();
            Mixer.removeSend(1, 0);
            Project.tick();
            check(Project.undoText === "Remove Send", "remove Lead's send");
            Project.undo();
        },
        () => {
            // The mixer has picked up the undo on its clock.
            shoot(window.title, "mix-routing");
        },
        () => {
            // Click the compressor added above: its panel opens.
            const id = Mixer.insertDevice(0, 0);
            check(id !== "" && DevicePanel.open(id), "open the compressor's panel");
            check(DevicePanel.active && DevicePanel.deviceKind === "daw.compressor", "panel shows the compressor");
            check(DevicePanel.paramCount > 0, "compressor parameters listed");
            check(!PluginEditors.hasEditor(id), "a built-in device has no plugin interface");
            // Drag the threshold: one undo step.
            Project.tick();
            const before = Project.undoText;
            DevicePanel.setValue(0, -30, true);
            DevicePanel.setValue(0, -24, true);
            DevicePanel.endGesture();
            // Project's properties follow the document on its tick.
            Project.tick();
            check(Project.undoText === "Change Parameter", "parameter edit undoes");
            Project.undo();
            check(Project.undoText === before, "a drag is one undo step");
            Project.redo();
            DevicePanel.setDeviceEnabled(false);
            Project.tick();
            check(!DevicePanel.switchedOn && Project.undoText === "Bypass Device", "bypass");
            DevicePanel.setDeviceEnabled(true);
            check(DevicePanel.switchedOn, "switch back on");
        },
        () => {
            // Beside the page on a wide window, the panel takes room from
            // it rather than covering its toolbar.
            if (window.wideScreen) {
                // The drawer slides in; a slow machine may not have
                // started the animation yet.
                const panel = window.devicePanelDrawer;
                if (window.pageStack.anchors.rightMargin === 0 && test.waits < 10) {
                    return false;
                }
                test.waits = 0;
                check(window.pageStack.anchors.rightMargin > 0,
                      "device panel beside the page (modal " + panel.modal + ", visible " + panel.visible
                      + ", position " + panel.position + ")");
            }
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
            // The plugin browser, as Add Effect opens it.
            test.browser = window.browsePlugins(2, (format, pluginId, name) => {
                test.picked = [format, pluginId, name];
                check(Mixer.addPluginInsert(0, format, pluginId, name), "insert the chosen plugin");
            });
            check(test.browser.visible, "plugin browser opens");
            check(Plugins.scanning || Plugins.hasScanned, "plugin scan started");
        },
        () => {
            // Give the background scan a while (smoke.rs points it at the
            // test plugin only).
            if (Plugins.scanning && test.waits < 50) {
                return false;
            }
            // Later steps get their own budget.
            test.waits = 0;
            check(Plugins.hasScanned, "plugin scan finished");
            shoot(window.title, "plugin-browser");
            if (!window.smokeTest) {
                return;
            }
            // smoke.rs puts the CLAP test plugin in CLAP_PATH; a packaged
            // smoke test runs with whatever plugins the system has.
            Plugins.setFilter("test gain", 1, 2);
            if (Plugins.count === 0) {
                console.info("smoke test: no test plugin; skipping the plugin checks");
                return;
            }
            console.info("smoke test: checking the test plugin");
            check(Plugins.count === 1, "search finds it");
            Plugins.setFilter("test gain", 0, 1);
            check(Plugins.count === 0, "it is not an instrument");
            Plugins.setFilter("test gain", 1, 2);
            test.browser.pick(0);
            check(test.picked !== null && test.picked[0] === "clap" && test.picked[2] === "DAW Test Gain",
                  "picking a plugin reports it");
            check(!test.browser.opened, "the browser closes on a pick");
        },
        () => {
            test.browser.close();
            if (window.smokeTest && test.picked !== null) {
                Project.tick();
                check(Project.undoText === "Add Device", "the plugin was inserted");
                Project.undo();
            }
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
            // Dragging the velocity lane's edge up makes it taller, but
            // never so tall the notes lose their last three rows.
            const velocity = window.pageStack.currentItem.velocitySize;
            const savedVelocity = velocity.units;
            velocity.begin(roll.velocityHeight, roll.maximumVelocityHeight);
            velocity.drag(Kirigami.Units.gridUnit, false);
            check(Math.abs(roll.velocityHeight - Math.round(velocity.pixels)) < 1, "resize the velocity lane");
            velocity.begin(roll.velocityHeight, roll.maximumVelocityHeight);
            velocity.drag(100000, false);
            check(velocity.pixels <= roll.maximumVelocityHeight + 0.5
                  && roll.velocityHeight === Math.round(Math.min(velocity.pixels, roll.maximumVelocityHeight)),
                  "the velocity lane leaves room for notes");
            velocity.reset();
            check(velocity.units === velocity.defaultUnits, "velocity lane reset");
            velocity.units = savedVelocity;
            AppSettings.setLayoutValue(velocity.key, savedVelocity);
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
            // The export ran in the background meanwhile; give a slow
            // machine more time.
            if (Exporter.running && test.waits < 75) {
                return false;
            }
            check(!Exporter.running, "export finished");
            check(exportMessage.context === Project.ExportContext && exportMessage.reason === Project.Exported,
                  "export reported done: " + projectMessage.text(exportMessage));
            check(TestHelper.fileExists(exportUrl + ".wav"), "exported file has its extension");
            console.info("export message:", projectMessage.text(exportMessage));
            // Nothing to export from an empty range: refused up front.
            const refused = Exporter.start(exportUrl, Exporter.Bars, 3, 2, 0, 0, 48000, false);
            check(refused.context === Project.ExportContext && refused.reason === Project.EmptyRange,
                  "empty range refused");
            window.openExport().close();
        },
        () => {
            // A project with a file outside it and a missing one.
            filesDir = TestHelper.makeFilesTest();
            check(filesDir !== "", "make the files test project");
            const url = "file://" + filesDir + "/Files";
            const result = Project.openProject(url);
            check(result.context === undefined, "open the files test project");
            check(Project.recentProjects.length === 1 && Project.recentProjects[0] === filesDir + "/Files",
                  "an opened project is recent");
            check(Project.missingCount === 1 && Project.missingFiles[0] === filesDir + "/moved/snare.wav",
                  "missing file found");
            check(Project.missingClipCount(0) === 1, "missing file's clips counted");
            check(Project.externalCount === 1, "file outside the project found");
            window.opened(result, url);
            check(window.relinkDialog.visible, "relink dialog offered");
        },
        () => {
            shoot(window.title, "relink");
        },
        () => {
            check(Project.relink(0, "file://" + filesDir + "/found/snare.wav") === 1, "relink");
            check(Project.missingCount === 0, "nothing missing after relink");
            check(!window.relinkDialog.opened, "relink dialog closes when nothing is missing");
            check(Project.undoText === "Relink Audio" && Project.undoText in editNames.names, "relink undoes");
            check(Project.externalCount === 2, "both files are outside the project");
            check(Project.collectFiles() === 2, "collect copies both files");
            check(Project.externalCount === 0, "nothing outside the project after collect");
            check(Project.undoText === "Collect Audio Files" && Project.undoText in editNames.names,
                  "collect undoes");
            Project.undo();
            check(Project.externalCount === 2, "undo collect");
            Project.redo();
            check(Project.externalCount === 0, "redo collect");
            // Unsaved changes: replacing the project asks first.
            check(Project.modified, "collect is an unsaved change");
            window.fileActions[0].trigger();
            check(window.unsavedDialog.visible, "New asks about unsaved changes");
        },
        () => {
            shoot(window.title, "unsaved");
        },
        () => {
            window.unsavedDialog.reject();
            check(Project.trackCount === 1, "cancel keeps the project");
            window.fileActions[0].trigger();
            window.unsavedDialog.discarded();
            check(!window.unsavedDialog.opened, "discard closes the question");
            check(Project.trackCount === 0 && !Project.modified, "discard makes the new project");
            check(!TestHelper.fileExists("file://" + filesDir + "/Files/Files.kadence.autosave"),
                  "discarding removes the autosave");
        },
        () => {
            // The welcome placeholder lists the recent project.
            shoot(window.title, "welcome");
            check(TestHelper.offerTestRecovery(), "leave unsaved work behind");
            Project.tick();
            check(Project.recoveryCount === 1 && Project.recoveryName(0) === "", "unsaved untitled work offered");
            window.recoveryDialog.open();
        },
        () => {
            shoot(window.title, "recover");
        },
        () => {
            const result = Project.recoverSession(0);
            check(result.context === undefined, "recover");
            check(Project.trackCount === 1 && Project.trackName(0) === "Recovered", "recovered tracks");
            check(Project.modified && !Project.savedBefore, "recovered work is unsaved");
            check(Project.recoveryCount === 0 && !window.recoveryDialog.opened, "nothing left to recover");
            // Open Recent goes through the same question.
            window.openRecent(0);
            check(window.unsavedDialog.visible, "Open Recent asks about unsaved changes");
            window.unsavedDialog.discarded();
            check(Project.name === "Files" && Project.trackCount === 1, "open recent");
        },
        () => {
            settingsPage = window.openSettings();
            check(settingsPage !== null, "open settings");
        },
        () => {
            check(settingsPage.currentTab === 0, "settings open on General");
            shoot(settingsPage.Window.window.title, "settings-general");
            settingsPage.currentTab = 1;
        },
        () => {
            shoot(settingsPage.Window.window.title, "settings");
            settingsPage.currentTab = 2;
        },
        () => {
            // The stub offers two devices and a program: hardware is used
            // by default, programs only when turned on.
            const keys = "Midi-Bridge:USB Keyboard (capture)";
            if (Engine.backendKind === Engine.StubBackend) {
                check(AppSettings.midiSourceNames.includes(keys), "stub MIDI device listed");
                check(AppSettings.midiInputNames.includes(keys), "hardware used by default");
                check(!AppSettings.midiInputNames.includes("Sequencer:out"), "programs not used by default");
                check(AppSettings.midiLabel(keys) === "USB Keyboard (capture)", "MIDI label without the program");
                AppSettings.useMidiSource("Sequencer:out", true);
                check(AppSettings.midiInputNames.includes("Sequencer:out"), "turn a program on");
                AppSettings.useMidiSource("Sequencer:out", false);
                check(!AppSettings.midiInputNames.includes("Sequencer:out"), "and off again");
            }
            shoot(settingsPage.Window.window.title, "settings-midi");
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
            check(window.aboutAction.checked && !window.pageActions[0].checked,
                  "the drawer marks About while it is open");
            // Picking About again (which unchecks it first) keeps it open
            // and marked.
            window.aboutAction.trigger();
            check(window.pageStack.layers.depth === 2 && window.aboutAction.checked,
                  "picking About again keeps it open and marked");
            // What Escape does.
            window.closeAbout();
            check(window.pageStack.layers.depth === 1 && !window.aboutAction.checked
                  && window.pageActions[0].checked,
                  "closing About marks the page again");
            window.closeAbout();
            check(window.pageStack.layers.depth === 1, "closing About twice is harmless");
            window.openAbout();
        },
        () => {
            // Picking a page in the drawer leaves About.
            window.pageActions[0].trigger();
            check(window.pageStack.layers.depth === 1, "a page action closes About");
            check(window.pageActions[0].checked && !window.aboutAction.checked,
                  "the drawer marks the page again after About");
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
                    // A step returning false isn't ready: run it again.
                    if (test.steps[current]() === false) {
                        test.step = current;
                        test.waits += 1;
                    }
                } catch (error) {
                    test.check(false, "step " + current + " threw: " + error);
                }
                return;
            }
            stop();
            console.info("smoke test:", test.failures === 0 ? "passed" : "FAILED");
            TestHelper.removeTempFiles();
            Qt.exit(test.failures === 0 ? 0 : 3);
        }
    }
}
