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
// autosave), the transport (the playhead), the plugin scan, a running
// export, then `ticked` for views; on every third beat `slow`, and about
// once a second the audio engine's health and the MIDI devices.
// While the transport runs the clock also beats `frame` at the display's
// pace, after the beat's own work, so a view draws what the beat read.
//
// It also says in which order a change to the project is shown: see
// `revised`.
//
// It runs from its first use, which is the pages' own (they listen to it).
QtObject {
    id: clock

    // The project changed, whoever changed it (`Project.revision` or
    // `Automation.revision` moved). Tracks, Transport and Arrangement have
    // followed already, so a view that listens here lays out against the
    // tracks, lanes and selection as they are now. Views redraw from this,
    // not from the two revisions.
    signal revised()

    // About 30 times a second: `seconds` since the last one. For meters
    // and controls that follow playback.
    signal ticked(real seconds)
    // About ten times a second, after `ticked`: for state that changes
    // behind the app's back and need not look live (a plugin's own window).
    signal slow()
    // Every display frame while the transport runs, after the beat: for
    // views that draw the playhead.
    signal frame()

    // A beat in milliseconds, and how many beats make the slower turns.
    readonly property int beat: 33
    readonly property int slowBeats: 3
    readonly property int midiBeats: 30

    property Timer timer: Timer {
        // Counted, not timed: the wall clock can step back, and the beat
        // must go on.
        property int triggers: 0
        property int beats: 0
        property double lastBeat: Date.now()
        // Twice a beat while playing, for `frame`.
        interval: Transport.playing ? Math.floor(clock.beat / 2) : clock.beat
        repeat: true
        running: true
        onTriggered: {
            ++triggers;
            if (!Transport.playing || triggers % 2 === 0) {
                const now = Date.now();
                // Never backwards, and no leap after a stall or a clock
                // change: meters fall by this much.
                const seconds = Math.min(Math.max(now - lastBeat, 0), 250) / 1000;
                lastBeat = now;
                ++beats;
                Project.tick();
                Transport.sync();
                Plugins.tick();
                if (Exporter.running) {
                    Exporter.poll();
                }
                clock.ticked(seconds);
                if (beats % clock.slowBeats === 0) {
                    clock.slow();
                }
                // The audio engine's health, then MIDI devices plugged in
                // or out: in this order, so the MIDI ports are made on the
                // audio that is left after a restart.
                if (beats % clock.midiBeats === 0) {
                    Engine.poll();
                    AppSettings.refreshMidi();
                }
            }
            if (Transport.playing) {
                clock.frame();
            }
        }
    }

    // A change is shown in this order: the track list, the transport and
    // the arrangement, then everything that listens to `revised`. Transport
    // and Arrangement follow the project only through here (and Transport
    // on every beat as well).
    function revise(): void {
        Tracks.sync();
        Transport.sync();
        Arrangement.sync();
        clock.revised();
    }
    property Connections projectChanges: Connections {
        target: Project
        function onRevisionChanged() { clock.revise(); }
    }
    property Connections laneChanges: Connections {
        target: Automation
        function onRevisionChanged() { clock.revise(); }
    }
    // An edit made through another bridge shows everywhere at once, not on
    // the next beat: the project's revision moves, and `revise` follows.
    property Connections trackEdits: Connections {
        target: Tracks
        function onEdited() { Project.sync(); }
    }
    property Connections transportEdits: Connections {
        target: Transport
        function onEdited() { Project.sync(); }
    }
    property Connections arrangementEdits: Connections {
        target: Arrangement
        function onEdited() { Project.sync(); }
    }
}
