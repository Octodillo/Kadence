// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// Shows an instrument track's instrument and opens a menu of the
// instruments it can play instead (the rows of Instruments).
QQC2.ToolButton {
    id: button

    // The instrument's name; empty when the track has none.
    property string instrument
    // Its row in Instruments, or -1; that row shows checked.
    property int currentChoice: -1

    // The user picked row `choice` of Instruments.
    signal chosen(int choice)

    icon.name: "audio-midi"
    text: instrument || i18nc("@action:button the track has no instrument", "Choose Instrument")
    font: Kirigami.Theme.smallFont
    display: QQC2.AbstractButton.TextBesideIcon
    Accessible.name: instrument
        ? i18nc("@action:button %1 is an instrument name", "Instrument: %1", instrument)
        : text
    QQC2.ToolTip.text: Accessible.name
    QQC2.ToolTip.visible: hovered
    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

    onClicked: {
        Instruments.refresh();
        menu.popup(button, 0, button.height);
    }

    QQC2.Menu {
        id: menu
        Repeater {
            model: Instruments
            delegate: QQC2.MenuItem {
                required property int index
                required property string name
                text: name
                checkable: true
                checked: index === button.currentChoice
                onTriggered: button.chosen(index)
            }
        }
    }
}
