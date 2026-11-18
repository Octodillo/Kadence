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
    // The Edit strip's row before a track is put in front of it.
    property int stripRow: -1
    // A strip as the demo has it (see Mixer.get), to compare with later.
    property var demoStrip: ({})
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
            check(!window.devicePanelDrawer.drawerOpen, "no device panel at start");
            check(Array.from(window.pageStack.currentItem.actions).includes(window.recordAction),
                  "Record button on the Arrange toolbar");
            shoot(window.title, "arrange");
        },
        () => {
            const arrange = window.pageStack.currentItem;
            check(Tracks.count === 0, "new project is empty");
            arrange.addTrack(Tracks.InstrumentTrack);
            check(Tracks.count === 1, "add instrument track");
            check(Tracks.get(0).name === i18nc("@item default track name, %1 is a number", "Instrument %1", 1),
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
            // A list view makes its delegates when it next lays out.
            arrange.trackHeaders.forceLayout();
            const header = arrange.trackHeaders.itemAtIndex(0);
            check(header.edgeChange(12, 100, 2, 1, 10, 2000, 500) === 12, "a dragged edge follows the pointer");
            check(header.edgeChange(12, 0, 2, 1, 3, 192, 500) === 4 && header.edgeChange(12, 0, 1, 2, 3, 192, 500) === 4,
                  "in a short timeline, the sizes above a dragged edge share the movement");
            check(header.edgeChange(-30, 10, 1, 1, 10, 2000, 500) === -20, "an edge scrolls to the top, then shares the rest");
            // What the header's menu does is also a page action on the
            // selected track, for the keyboard.
            check(Arrangement.selectedTrack === 0 && arrange.trackActions[0].enabled, "a new track is selected");
            arrange.trackActions[0].trigger();
            check(header.renaming, "Rename Track opens the selected track's name");
            header.dismiss();
            check(!header.renaming && !arrange.trackActions[1].enabled && !arrange.trackActions[2].enabled,
                  "an only track has nowhere to move");
            // Double-click the first lane: a one-bar MIDI clip appears.
            v.doubleClick(v.width / 4, v.rulerHeight + v.trackHeight / 2, 0);
            check(Project.canUndo && Project.undoText !== "", "clip added");
            check(Project.undoText in editNames.names, "undo name has a translation");
            // Arm it and pick a MIDI input: both undoable, both translated.
            Tracks.setArmed(0, true);
            check(Tracks.get(0).armed && Project.undoText === "Arm Track", "arm a track");
            Tracks.setMidiInput(0, "Midi-Bridge:USB Keyboard (capture)", 9);
            check(Tracks.get(0).midiDevice === "Midi-Bridge:USB Keyboard (capture)"
                  && Tracks.get(0).midiChannel === 9
                  && Project.undoText === "Change MIDI Input"
                  && Project.undoText in editNames.names, "choose a MIDI input");
            Project.undo();
            check(Tracks.get(0).midiDevice === "" && Tracks.get(0).midiChannel === -1, "undo the MIDI input");
            Project.undo();
            check(!Tracks.get(0).armed, "undo arming");
            Project.undo();
            Project.undo();
            check(Tracks.count === 0, "undo back to empty");
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
            // fire before the application's clock has ticked.
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
            // Building the demo can outlast the clock's beat, and then this
            // timer may fire before it: refresh Project's properties now.
            Project.tick();
        },
        () => {
            // A slow load can block the event loop past this step's timer,
            // so this timer and the clock are overdue at once; refresh before reading.
            Project.tick();
            check(Tracks.count === 4, "demo has three tracks and a bus");
            // Show an armed instrument track listening to one channel.
            for (let i = 0; i < Tracks.count; i++) {
                if (Tracks.get(i).kind === Tracks.InstrumentTrack) {
                    Tracks.setArmed(i, true);
                    Tracks.setMidiInput(i, "", 0);
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
            Automation.setExpanded(0, true);
            Automation.setExpanded(2, true);
            // Under the track: its controls, and the headings of the groups
            // the rest fold into. A lane is drawn in its control's row.
            const rows = track => Tracks.get(track).rows;
            const volume0 = () => rows(0)[0];
            const count = rows(0).length;
            check(Tracks.get(0).expanded && count >= 4 && v.rowHeight(0) === v.trackHeight + count * v.laneHeight, "Bass shows its controls");
            check(volume0().kind === Automation.VolumeLane && volume0().lane === 0, "volume comes first, with the demo's lane");
            check(volume0().mode === Automation.LaneRead, "lanes read by default");
            check(rows(0)[1].kind === Automation.PanLane && rows(0)[1].lane === -1, "pan has no lane yet");
            const effects = rows(0).findIndex(r => r.kind === Automation.EffectsGroup);
            check(effects > 1 && !rows(0)[effects].open && rows(0)[effects].depth === 0, "Effects starts closed");
            Automation.setRowOpen(0, effects, true);
            check(rows(0)[effects].open && v.rowHeight(0) === v.trackHeight + rows(0).length * v.laneHeight, "Effects opens");
            Automation.setRowOpen(0, effects, false);
            check(rows(0).length === count, "Effects closes");
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
            check(lanes.units === lanes.defaultUnits && v.rowHeight(0) === v.trackHeight + count * v.laneHeight, "lane height reset");
            lanes.units = savedLanes;
            AppSettings.setLayoutValue(lanes.key, savedLanes);
            const y = v.rulerHeight + v.trackHeight + v.laneHeight / 2;
            v.press(v.width * 0.45, y, 1, 0);
            v.release(v.width * 0.45, y, 0);
            Project.tick();
            check(Project.undoText === "Edit Automation", "click adds an automation point");
            check(Project.undoText in editNames.names, "automation edit has a translation");
            // A click beside a control without a lane starts automating it.
            v.press(v.width * 0.45, y + v.laneHeight, 1, 0);
            v.release(v.width * 0.45, y + v.laneHeight, 0);
            Project.tick();
            check(rows(0)[1].lane === 1, "a click beside Pan starts its lane");
            Project.undo();
            Project.undo();
            check(rows(0)[1].lane === -1 && Project.undoText === "Edit Automation", "undo takes the new lane away");
            Automation.setRowMode(0, 0, Automation.LaneOff);
            Project.tick();
            check(volume0().mode === Automation.LaneOff && Project.undoText === "Toggle Automation", "lane off");
            // An off lane's control sets the fader; a playing one follows
            // its points and can't be moved.
            const off = Automation.rowControl(0, 0);
            check(!off.following, "an off lane doesn't follow its points");
            check(!off.writes, "a lane in read mode doesn't write");
            const volume = off.value;
            Automation.beginRowControl(0, 0);
            Automation.setRowPosition(0, 0, 0.25);
            Automation.setRowPosition(0, 0, 1);
            Automation.endRowControl(0, 0);
            Project.tick();
            const moved = Automation.rowControl(0, 0);
            check(moved.value > volume && moved.position === 1, "a row's control sets the volume");
            // The next gesture (a key press) is its own undo step.
            Automation.beginRowControl(0, 0);
            Automation.setRowPosition(0, 0, 0.5);
            Automation.endRowControl(0, 0);
            Project.undo();
            check(Automation.rowControl(0, 0).position === 1, "each control gesture is its own undo step");
            Project.undo();
            check(Automation.rowControl(0, 0).value === volume, "a control drag is one undo step");
            Automation.setRowMode(0, 0, Automation.LaneWrite);
            check(volume0().mode === Automation.LaneWrite && Automation.rowControl(0, 0).writes, "lane in write mode writes");
            Automation.setRowMode(0, 0, Automation.LaneRead);
            Project.undo();
            Project.undo();
            Project.undo();
            // For the picture: the Keys' instrument, opened to its
            // parameters.
            const instrument = rows(2).findIndex(r => r.kind === Automation.InstrumentGroup);
            const closed = rows(2).length;
            Automation.setRowOpen(2, instrument, true);
            const param = rows(2)[instrument + 1];
            check(instrument >= 0 && rows(2).length > closed && param.depth === 1
                  && param.kind === Automation.DeviceParamLane && param.detail === "",
                  "the instrument opens to its parameters");
            v.refresh();
        },
        () => {
            shoot(window.title, "arrange-automation");
        },
        () => {
            // The rows do what Bass's channel strip does: Effects adds an
            // effect, which is its own heading there, and Sends a send.
            const rows = () => Tracks.get(0).rows;
            const strip = Tracks.get(0).strip;
            check(strip >= 0 && Mixer.get(strip).name === Tracks.get(0).name, "a track knows its strip");
            const effects = rows().findIndex(r => r.kind === Automation.EffectsGroup);
            Automation.setRowOpen(0, effects, true);
            const sends = rows().findIndex(r => r.kind === Automation.SendsGroup);
            Automation.setRowOpen(0, sends, true);
            demoStrip = Mixer.get(strip);
            check(Mixer.addInsert(strip, 0), "add an effect from the Effects row");
            check(Mixer.get(strip).sendChoices.length > 0 && Mixer.addSend(strip, 0), "add a send from the Sends row");
        },
        () => {
            const rows = () => Tracks.get(0).rows;
            const strip = Tracks.get(0).strip;
            const inserts = Mixer.get(strip).inserts.length;
            const effect = rows().findIndex(r => r.kind === Automation.DeviceGroup && r.item === inserts - 1);
            check(effect >= 0 && rows()[effect].depth === 1, "the new effect is a heading under Effects");
            const send = rows().findIndex(r => r.kind === Automation.SendLane && r.item >= 0);
            check(send >= 0 && rows()[send].depth === 1 && rows()[send].lane === -1, "the new send's level is a row under Sends");
            // What the rows' Remove actions do.
            const arrange = window.pageStack.currentItem;
            arrange.trackHeaders.forceLayout();
            const header = arrange.trackHeaders.itemAtIndex(0)?.rowHeader(effect) ?? null;
            check(header !== null && header.removeAction.visible && header.settingsAction.visible, "an effect's heading can remove it");
            const sendItem = rows()[send].item;
            // The removal shows in the rows at once.
            header?.removeAction.trigger();
            check(Mixer.get(strip).inserts.length === inserts - 1
                  && rows().filter(r => r.kind === Automation.DeviceGroup).length === inserts - 1, "the effect's row goes with it");
            Mixer.removeSend(strip, sendItem);
        },
        () => {
            const strip = Tracks.get(0).strip;
            const rows = Tracks.get(0).rows;
            const now = Mixer.get(strip);
            check(now.sends.length === demoStrip.sends.length
                  && rows.filter(r => r.kind === Automation.SendLane).length === demoStrip.sends.length, "remove the send");
            check(now.inserts.length === demoStrip.inserts.length
                  && rows.filter(r => r.kind === Automation.DeviceGroup).length === demoStrip.inserts.length, "remove the effect");
            // Back as the demo was.
            for (let i = 0; i < 4; ++i) {
                Project.undo();
            }
            check(JSON.stringify(Mixer.get(strip).inserts) === JSON.stringify(demoStrip.inserts)
                  && Mixer.get(strip).sends.length === demoStrip.sends.length, "undo takes the effect and send back off");
            Automation.setRowOpen(0, rows.findIndex(r => r.kind === Automation.SendsGroup), false);
            const effects = Tracks.get(0).rows.findIndex(r => r.kind === Automation.EffectsGroup);
            Automation.setRowOpen(0, effects, false);
        },
        () => {
            Automation.setExpanded(0, false);
            Automation.setExpanded(2, false);
            check(!Tracks.get(0).expanded, "lanes hidden");
        },
        () => {
            // Tempo and time signature: edits in a row are one undo step.
            check(Transport.bpm === 120 && Transport.meterNumerator === 4 && Transport.meterDenominator === 4,
                  "demo starts at 120 bpm in 4/4");
            Transport.setTempo(96);
            Transport.setTempo(98.5);
            check(Transport.bpm === 98.5, "tempo set");
            check(Project.undoText === "Change Tempo" && Project.undoText in editNames.names, "tempo edit undoes");
            Transport.setTimeSignature(6, 7);
            check(Transport.meterNumerator === 6 && Transport.meterDenominator === 4, "time signature set, rounded");
            check(Project.undoText === "Change Time Signature" && Project.undoText in editNames.names,
                  "time signature edit undoes");
            Project.undo();
            check(Transport.meterNumerator === 4 && Transport.bpm === 98.5, "undo the time signature");
            Project.undo();
            check(Transport.bpm === 120, "undo the tempo drag in one step");
            Project.redo();
            check(Transport.bpm === 98.5, "redo");
            Transport.setTimeSignature(3, 4);
            // The metronome, with its settings open, for a screenshot.
            Transport.setMetronomeEnabled(true);
            Transport.setCountIn(1);
            check(Transport.metronome && Transport.countInBars === 1, "metronome settings");
            const arrange = window.pageStack.currentItem;
            arrange.transportBar.openClickSettings();
        },
        () => {
            shoot(window.title, "arrange-metronome");
            window.pageStack.currentItem.transportBar.closeClickSettings();
            Transport.setMetronomeEnabled(false);
            Transport.setCountIn(0);
            Project.undo();
            Project.undo();
            check(Transport.bpm === 120 && Transport.meterNumerator === 4, "back to the demo's tempo");
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
            Transport.toggleLoop();
            check(Transport.looping, "loop on");
            Transport.togglePlay();
        },
        () => {
            check(Transport.timeText !== "0:00.0", "playhead moves while playing");
            Transport.togglePlay();
            Transport.toggleLoop();
            shoot(window.title, "arrange-played");
        },
        () => {
            // The export's notification would cover the screenshots below.
            window.hidePassiveNotification();
            window.pageActions[1].trigger();
            check(window.pageStack.currentItem.title === i18nc("@title", "Mix"), "switch to Mix");
        },
        () => {
            // The mixer catches up with the project on its own clock, which
            // a loaded machine can take a few ticks for.
            // The demo's three tracks, its reverb bus and the master.
            if (Mixer.meterLevels().length !== 5 * 5 && test.waits < 25) {
                return false;
            }
            test.waits = 0;
            check(Mixer.meterLevels().length === 5 * 5, "a strip per track");
            const compressor = Mixer.effectNames.indexOf("Compressor");
            check(compressor >= 0 && Mixer.addInsert(0, compressor), "add a compressor to Bass");
            // Play so the meters have something to show.
            window.playAction.trigger();
        },
        () => {
            // The engine swaps in the new graph on its own time, and a busy
            // machine (CI runs several app instances at once) can take more
            // than one step to bring the compressor's meter up.
            const metered = Mixer.meterLevels().some((v, i) => i % 5 < 4 && v > 0) && Mixer.meterLevels()[0 * 5 + 4] >= 0;
            if (!metered && test.waits < 25) {
                return false;
            }
            test.waits = 0;
            check(Project.undoText === "Add Device", "mixer edits undo");
            check(Mixer.meterLevels().some((v, i) => i % 5 < 4 && v > 0), "meters move while playing");
            check(Mixer.meterLevels()[0 * 5 + 4] >= 0, "compressor gain reduction metered");
            Mixer.setSoloed(0, true);
            check(Mixer.anySoloed, "solo");
            Mixer.setSoloed(0, false);
            // Touch-write a fader move on Lead.
            Mixer.setWriting(true);
            Mixer.touch(1, Mixer.VolumeControl, -1, true);
            Mixer.setVolume(1, -12);
            shoot(window.title, "mix");
        },
        () => {
            Mixer.setVolume(1, -20);
            Mixer.touch(1, Mixer.VolumeControl, -1, false);
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
            check(!DevicePanel.sampler && DevicePanel.sampleName === "", "the synth takes no sample");
            DevicePanel.close();
            check(!DevicePanel.active, "panel closed");
        },
        () => {
            // The drawer closes with an animation, slow on a loaded machine.
            if (window.devicePanelDrawer.drawerOpen && test.waits < 25) {
                return false;
            }
            test.waits = 0;
            check(!window.devicePanelDrawer.drawerOpen, "device panel closed");
            // The plugin browser, as Add Effect opens it.
            test.browser = window.browsePlugins(Plugins.EffectPlugin, (format, pluginId, name) => {
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
            // What the plugin settings list: a group of folders per format.
            check(Plugins.formats.join(" ") === "CLAP VST3 LV2", "the plugin formats are named");
            check(Plugins.formatFolders(0).length === 0 && Plugins.formatFolders(4).length === 0,
                  "only the three formats have folders");
            shoot(window.title, "plugin-browser");
            if (!window.smokeTest) {
                return;
            }
            // smoke.rs puts the CLAP test plugin in CLAP_PATH; a packaged
            // smoke test runs with whatever plugins the system has.
            Plugins.setFilter("test gain", 1, Plugins.EffectPlugin);
            if (Plugins.count === 0) {
                console.info("smoke test: no test plugin; skipping the plugin checks");
                return;
            }
            console.info("smoke test: checking the test plugin");
            check(Plugins.count === 1, "search finds it");
            Plugins.setFilter("test gain", 0, Plugins.InstrumentPlugin);
            check(Plugins.count === 0, "it is not an instrument");
            Plugins.setFilter("test gain", 1, Plugins.EffectPlugin);
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
            // Beside the notes: the mixer strip of the clip's track (the
            // demo's bus comes last, so its rows are track numbers).
            const edit = window.pageStack.currentItem;
            const strip = edit.trackStrip.strip;
            check(strip !== null && strip.index === roll.stripRow && strip.kind === Mixer.TrackStrip
                  && strip.name === Tracks.get(roll.stripRow).name, "the Edit page shows the clip's track strip");
            check(strip !== null && strip.edge === Qt.LeftEdge
                  && Math.abs(edit.trackStrip.width - edit.stripWidth.pixels) < 1, "the track strip has its own width");
            const stripToggle = edit.editActions[21];
            stripToggle.trigger();
            check(!edit.stripShown && !edit.trackStrip.shown && edit.trackStrip.width === 0, "hide the track strip");
            stripToggle.trigger();
            check(edit.stripShown && edit.trackStrip.shown, "show the track strip again");
            // Under the velocity lane: the clip's track's automation lanes,
            // named beside them, toggled from the toolbar.
            const lanes = roll.laneCount;
            // Arming a control that has no lane for writing gives it one.
            Automation.setExpanded(roll.trackIndex, true);
            const free = Tracks.get(roll.trackIndex).rows.findIndex(r => r.kind <= Automation.DeviceParamLane && r.lane < 0);
            Automation.setRowMode(roll.trackIndex, free, Automation.LaneWrite);
            Automation.setRowMode(roll.trackIndex, free, Automation.LaneRead);
            roll.refresh();
            check(roll.laneCount === lanes + 1 && roll.lanesHeight > 0
                  && roll.lanesHeight <= roll.laneCount * roll.laneHeight,
                  "the Edit page shows the track's automation lanes");
            const lanesToggle = edit.editActions[23];
            lanesToggle.trigger();
            check(!edit.lanesShown && roll.lanesHeight === 0, "hide the automation lanes");
            lanesToggle.trigger();
            check(edit.lanesShown && roll.lanesHeight > 0, "show the automation lanes again");
            Project.undo();
            roll.refresh();
            check(roll.laneCount === lanes, "undo removes the lane");
            // A track in front moves the clip's strip a row on; the Mixer
            // resets its rows, and the strip must survive that.
            test.stripRow = roll.stripRow;
            Tracks.add(Tracks.InstrumentTrack, "Strip Test");
            for (let i = 0; i < Tracks.count; i++) {
                if (Tracks.get(i).name === "Strip Test") {
                    Tracks.moveTrack(i, 0);
                    break;
                }
            }
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
            // Double-click empty space low in the grid: a note is added,
            // and dragging before letting go sets its length in the same
            // undo step.
            const newX = roll.width * 0.8;
            const newY = roll.height - roll.lanesHeight - roll.velocityHeight - 20;
            roll.press(newX, newY, Qt.LeftButton, 0);
            roll.doubleClick(newX, newY, 0);
            check(TestHelper.pianoNoteCount() === notes + 1, "double-click adds a note");
            roll.motion(newX + Kirigami.Units.gridUnit * 3, newY, 0);
            roll.release(newX + Kirigami.Units.gridUnit * 3, newY, 0);
            check(TestHelper.pianoNoteCount() === notes + 1, "dragging the new note keeps it");
            Project.undo();
            check(TestHelper.pianoNoteCount() === notes, "undo removes it");
            // The toolbar spells the black keys either way.
            const flatsToggle = edit.editActions[24];
            const flats = AppSettings.flatNoteNames;
            flatsToggle.trigger();
            check(AppSettings.flatNoteNames === !flats && roll.flatNoteNames === !flats, "toggle flat note names");
            flatsToggle.trigger();
            check(AppSettings.flatNoteNames === flats && roll.flatNoteNames === flats, "toggle note names back");
            roll.selectAll();
            check(roll.hasSelection, "select all");
            shoot(window.title, "edit");
        },
        () => {
            const edit = window.pageStack.currentItem;
            const strip = edit.trackStrip.strip;
            // The strip is chosen again just after each Mixer reset, which
            // the Mixer's own clock can trigger at any moment.
            if ((strip === null || strip.index !== edit.roll.stripRow) && test.waits < 10) {
                return false;
            }
            test.waits = 0;
            check(edit.roll.stripRow === test.stripRow + 1,
                  "the clip's track moved a row on (" + test.stripRow + " to " + edit.roll.stripRow + ")");
            check(strip !== null && strip.index === edit.roll.stripRow
                  && strip.name === Tracks.get(edit.roll.stripRow).name,
                  "the strip follows its track to another row (strip "
                  + (strip === null ? "none" : strip.index + " " + strip.name) + ")");
            Project.undo();
            Project.undo();
        },
        () => {
            const edit = window.pageStack.currentItem;
            const strip = edit.trackStrip.strip;
            check(edit.roll.stripRow === test.stripRow && strip !== null && strip.index === test.stripRow,
                  "the strip follows undo");
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
            check(Transport.playing, "play");
            window.playAction.trigger();
            check(!Transport.playing, "stop");
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
            check(Tracks.count === 1, "cancel keeps the project");
            window.fileActions[0].trigger();
            window.unsavedDialog.discarded();
            check(!window.unsavedDialog.opened, "discard closes the question");
            check(Tracks.count === 0 && !Project.modified, "discard makes the new project");
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
            check(Tracks.count === 1 && Tracks.get(0).name === "Recovered", "recovered tracks");
            check(Project.modified && !Project.savedBefore, "recovered work is unsaved");
            check(Project.recoveryCount === 0 && !window.recoveryDialog.opened, "nothing left to recover");
            // Open Recent goes through the same question.
            window.openRecent(0);
            check(window.unsavedDialog.visible, "Open Recent asks about unsaved changes");
            window.unsavedDialog.discarded();
            check(Project.name === "Files" && Tracks.count === 1, "open recent");
            // A take in progress is unsaved work too, though the project
            // itself has no changes yet.
            Tracks.setArmed(0, true);
            check(Project.save().context === undefined && !Project.modified, "save with the track armed");
            Transport.toggleRecord();
            check(Transport.recording && !Project.modified, "a take in progress is not in the project yet");
        },
        () => {
            // Until the take holds some audio.
            if (Transport.timeText === "0:00.0" && test.waits < 75) {
                return false;
            }
            window.fileActions[0].trigger();
            check(!Transport.recording && Project.undoText === "Record Audio", "New ends the take and keeps it");
            check(window.unsavedDialog.visible, "New asks about the take");
            window.unsavedDialog.reject();
            check(Project.name === "Files" && Tracks.count === 1, "cancel keeps the project and the take");
            Project.undo();
            check(!Project.modified, "without the take nothing is unsaved");
        },
        () => {
            settingsPage = window.openSettings(SettingsPage.Tab.General);
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
