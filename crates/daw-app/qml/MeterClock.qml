// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

import io.github.octodillo.daw

// Moves the mixer's meters on with the application's clock while
// `running`. A page that shows strips has one, running while the page is
// on screen.
Connections {
    property bool running: false
    target: UiClock
    enabled: running
    function onTicked(seconds) { Mixer.tick(seconds); }
}
