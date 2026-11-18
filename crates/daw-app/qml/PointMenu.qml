// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2

import io.github.octodillo.daw

// The context menu of an automation lane: the selected points' segment
// shape, and deleting them. Shared by the arrange view and the piano roll,
// which act on the choice for their own lanes.
QQC2.Menu {
    id: menu

    // The selected points' shape (an Automation.PointShape), -1 when the
    // menu is for the lane rather than a point.
    property int shape: -1

    signal shapeChosen(int shape)
    signal deleteChosen()

    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Linear")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === Automation.LinearShape
        onTriggered: menu.shapeChosen(Automation.LinearShape)
    }
    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Step")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === Automation.StepShape
        onTriggered: menu.shapeChosen(Automation.StepShape)
    }
    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Slow Start")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === Automation.SlowStartShape
        onTriggered: menu.shapeChosen(Automation.SlowStartShape)
    }
    QQC2.MenuItem {
        text: i18nc("@action:inmenu automation segment shape", "Fast Start")
        enabled: menu.shape >= 0
        checkable: true
        checked: menu.shape === Automation.FastStartShape
        onTriggered: menu.shapeChosen(Automation.FastStartShape)
    }
    QQC2.MenuSeparator {}
    QQC2.MenuItem {
        text: i18nc("@action:inmenu", "Delete Point")
        icon.name: "edit-delete"
        enabled: menu.shape >= 0
        onTriggered: menu.deleteChosen()
    }
}
