// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

import io.github.octodillo.daw

// Sets the metronome's level, in half decibels. Wherever it shows, it
// follows the project's setting.
QQC2.Slider {
    id: slider

    // MetronomeSettings::default() in daw-session.
    readonly property real defaultLevel: -6
    // The level as it is stored.
    readonly property real level: Math.round(value * 2) / 2

    // Sets the level to `db`; `save` keeps it as the user's setting.
    function setLevel(db: real, save: bool): void {
        Project.setMetronomeLevel(Math.round(db * 2) / 2, save);
    }

    from: -40
    to: 6
    value: Project.metronomeVolume
    // Saved once the handle is let go, not on every move.
    onMoved: setLevel(value, !pressed)
    onPressedChanged: {
        if (!pressed) {
            setLevel(value, true);
        }
    }
    Accessible.name: i18nc("@info:accessible", "Metronome level")

    // Moving the handle replaces the binding above; follow the project (the
    // other level slider) from here.
    Connections {
        target: Project
        function onMetronomeVolumeChanged() { slider.value = Project.metronomeVolume; }
    }
}
