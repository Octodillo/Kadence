// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

import io.github.octodillo.daw

// Chooses how many bars the metronome counts in before recording. Wherever
// it shows, it follows the project's setting.
QQC2.ComboBox {
    id: box

    // The entry at row n counts in n bars.
    model: [
        i18nc("@item:inlistbox no count-in", "None"),
        i18ncp("@item:inlistbox count-in length", "%1 bar", "%1 bars", 1),
        i18ncp("@item:inlistbox count-in length", "%1 bar", "%1 bars", 2)
    ]
    currentIndex: Transport.countInBars
    onActivated: (index) => Transport.setCountIn(index)
    Accessible.name: i18nc("@label:listbox", "Count-in before recording")

    // Using the box replaces the binding above; follow the project (the
    // other count-in box, undo) from here.
    Connections {
        target: Transport
        function onCountInBarsChanged() { box.currentIndex = Transport.countInBars; }
    }
}
