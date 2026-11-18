// SPDX-License-Identifier: GPL-3.0-or-later

pragma Singleton

import QtQuick

import io.github.octodillo.daw

// The application's one clock. Everything that is polled is polled from
// here, in the order written here; views that show polled state listen to
// the signals below while they are on screen rather than keep a timer of
// their own.
//
// Each beat, in order: the project (engine position, peak loading,
// autosave), the plugin scan, a running export, then `ticked` for views.
// While the transport runs the clock also beats `frame` at the display's
// pace, after the beat's own work, so a view draws what the beat read.
QtObject {
    id: clock

    // About 30 times a second: `seconds` since the last one. For meters
    // and controls that follow playback.
    signal ticked(real seconds)
    // About ten times a second, after `ticked`: for state that changes
    // behind the app's back and need not look live (a plugin's own window).
    signal slow()
    // Every display frame while the transport runs, after the beat: for
    // views that draw the playhead.
    signal frame()

    // Starts the clock; the application window does, once. (A singleton
    // exists only from its first use.)
    function start(): void {
        timer.start();
    }

    readonly property int beat: 33
    readonly property int slowBeat: 100

    property Timer timer: Timer {
        property double lastBeat: Date.now()
        property double lastSlow: lastBeat
        interval: Project.playing ? 16 : clock.beat
        repeat: true
        onRunningChanged: lastBeat = lastSlow = Date.now()
        onTriggered: {
            const now = Date.now();
            // A little early counts: timers fire to the millisecond.
            if (now - lastBeat >= clock.beat - 3) {
                const seconds = (now - lastBeat) / 1000;
                lastBeat = now;
                Project.tick();
                Plugins.tick();
                if (Exporter.running) {
                    Exporter.poll();
                }
                clock.ticked(seconds);
                if (now - lastSlow >= clock.slowBeat - 3) {
                    lastSlow = now;
                    clock.slow();
                }
            }
            if (Project.playing) {
                clock.frame();
            }
        }
    }

    // The track list follows the project whoever changed it and whichever
    // page is showing, before any view redraws for the same change.
    property Connections projectChanges: Connections {
        target: Project
        function onRevisionChanged() { Tracks.sync(); }
    }
    property Connections laneChanges: Connections {
        target: Automation
        function onRevisionChanged() { Tracks.sync(); }
    }
    // An edit in a track header shows everywhere at once, not on the next
    // beat.
    property Connections trackEdits: Connections {
        target: Tracks
        function onEdited() { Project.tick(); }
    }
}
