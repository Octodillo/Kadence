// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The header of one automation lane under a track: what it controls, its
// mode (off, read, write) and a button to remove it. `lane` is -1 for the
// empty row of a track without lanes, which offers to add one.
Rectangle {
    id: laneHeader

    property int track: -1
    property int lane: -1
    property int revision: 0

    readonly property bool placeholder: lane < 0
    // `revision >= 0 ?` makes the bindings depend on it (the QML compiler
    // drops a bare `revision;` statement).
    readonly property string laneText: revision >= 0 && !placeholder
        ? laneName.text(Automation.laneKind(track, lane),
                        Automation.laneDetail(track, lane),
                        Automation.laneParam(track, lane))
        : ""
    readonly property int mode: revision >= 0 && !placeholder ? Automation.laneMode(track, lane) : 1

    // Like the timeline's lane rows: a shade off the background.
    color: Kirigami.ColorUtils.tintWithAlpha(Kirigami.Theme.backgroundColor, Kirigami.Theme.textColor, 0.04)

    Accessible.name: placeholder ? i18nc("@info:accessible", "No automation lanes") : laneText

    LaneName {
        id: laneName
    }

    RowLayout {
        anchors.fill: parent
        // Indented past the track's colour, handle and expand button.
        anchors.leftMargin: Kirigami.Units.gridUnit * 2
        anchors.rightMargin: Kirigami.Units.smallSpacing
        spacing: Kirigami.Units.smallSpacing

        QQC2.ToolButton {
            visible: laneHeader.placeholder
            Layout.alignment: Qt.AlignVCenter
            icon.name: "list-add"
            text: i18nc("@action:button", "Add Lane")
            onClicked: addMenu.popup(this, 0, height)
        }

        QQC2.Label {
            visible: !laneHeader.placeholder
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            text: laneHeader.laneText
            elide: Text.ElideRight
            font: Kirigami.Theme.smallFont
            opacity: laneHeader.mode === 0 ? 0.6 : 1
        }

        Item {
            visible: laneHeader.placeholder
            Layout.fillWidth: true
        }

        QQC2.ComboBox {
            id: modeBox
            visible: !laneHeader.placeholder
            Layout.alignment: Qt.AlignVCenter
            Layout.maximumWidth: Kirigami.Units.gridUnit * 4.5
            flat: true
            font: Kirigami.Theme.smallFont
            model: [
                i18nc("@item:inlistbox automation lane mode", "Off"),
                i18nc("@item:inlistbox automation lane mode", "Read"),
                i18nc("@item:inlistbox automation lane mode", "Write")
            ]
            currentIndex: laneHeader.mode
            onActivated: (i) => Automation.setLaneMode(laneHeader.track, laneHeader.lane, i)
            Accessible.name: i18nc("@label:listbox", "Automation mode")
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Off: the control keeps its own setting. Read: the lane plays. Write: the lane plays, and moving the control on the Mix page while playing records over it.")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        QQC2.ToolButton {
            visible: !laneHeader.placeholder
            Layout.alignment: Qt.AlignVCenter
            icon.name: "list-remove"
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button", "Remove Lane")
            onClicked: Automation.removeLane(laneHeader.track, laneHeader.lane)
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
    }

    Kirigami.Separator {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        opacity: 0.5
    }

    AddLaneMenu {
        id: addMenu
        track: laneHeader.track
    }

    QQC2.Menu {
        id: laneMenu
        AddLaneMenu {
            track: laneHeader.track
        }
        QQC2.MenuItem {
            visible: !laneHeader.placeholder
            text: i18nc("@action:inmenu", "Remove Lane")
            icon.name: "list-remove"
            onTriggered: Automation.removeLane(laneHeader.track, laneHeader.lane)
        }
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Hide Automation")
            icon.name: "arrow-down"
            onTriggered: Automation.setExpanded(laneHeader.track, false)
        }
    }

    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: laneMenu.popup()
    }
}
