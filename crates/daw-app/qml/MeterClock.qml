// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

import io.github.octodillo.daw

// Moves the mixer's meters on, about 30 times a second while running. A
// page that shows strips has one, running while the page is on screen.
Timer {
    interval: 33
    repeat: true
    property double last: Date.now()
    onRunningChanged: last = Date.now()
    onTriggered: {
        const now = Date.now();
        Mixer.tick(Project.playing, (now - last) / 1000);
        last = now;
    }
}
