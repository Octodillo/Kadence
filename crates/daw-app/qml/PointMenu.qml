// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

// The context menu of an automation lane: the selected points' segment
// shape, and deleting them. Shared by the arrange view and the piano roll,
// which act on the choice for their own lanes.
QQC2.Menu {
    id: menu

    // The selected points' shape (0 linear, 1 step, 2 slow start, 3 fast
    // start), -1 when the menu is for the lane rather than a point.
    property int shape: -1

    signal shapeChosen(int shape)
    signal deleteChosen()

    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Linear")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === 0
        onTriggered: menu.shapeChosen(0)
    }
    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Step")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === 1
        onTriggered: menu.shapeChosen(1)
    }
    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Slow Start")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === 2
        onTriggered: menu.shapeChosen(2)
    }
    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Fast Start")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === 3
        onTriggered: menu.shapeChosen(3)
    }
    QQC2.MenuSeparator {}
    QQC2.MenuItem {
        text: i18nc("@action:inmenu", "Delete Point")
        icon.name: "edit-delete"
        enabled: menu.shape >= 0
        onTriggered: menu.deleteChosen()
    }
}
