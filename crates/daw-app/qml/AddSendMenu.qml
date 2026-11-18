// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

import io.github.octodillo.daw

// The buses the Mixer's strip `row` can get a send to; `choices` are their
// names (the strip's sendChoices). Picking one adds the send.
QQC2.Menu {
    id: menu

    required property int row
    required property list<string> choices

    Repeater {
        model: menu.choices
        delegate: QQC2.MenuItem {
            required property int index
            required property string modelData
            text: modelData
            onTriggered: Mixer.addSend(menu.row, index)
        }
    }
}
