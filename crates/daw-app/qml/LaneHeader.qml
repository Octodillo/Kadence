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
        ? laneName.text(kind,
                        Automation.laneDetail(track, lane),
                        Automation.laneParam(track, lane))
        : ""
    readonly property int mode: revision >= 0 && !placeholder ? Automation.laneMode(track, lane) : 1

    // What the lane controls, as it is now: kind (see Automation.laneKind),
    // value in its unit, and whether the lane's points set it as it plays.
    readonly property int kind: revision >= 0 && !placeholder ? Automation.laneKind(track, lane) : -1
    property real value: 0
    property bool following: false
    // Moving the control during playback records over the lane.
    property bool writes: false
    // How a device parameter's value is worded (see ValueText.param).
    readonly property string paramControl: kind === 3 && revision >= 0 ? Automation.laneControlKind(track, lane) : ""
    readonly property string paramUnit: kind === 3 && revision >= 0 ? Automation.laneUnit(track, lane) : ""
    readonly property var paramChoices: kind === 3 && revision >= 0 ? Automation.laneChoices(track, lane) : []

    function readValue(): void {
        if (placeholder) {
            return;
        }
        value = Automation.laneValue(track, lane);
        following = Automation.laneFollowing(track, lane);
        writes = Automation.laneWrites(track, lane);
        if (!levelSlider.pressed) {
            levelSlider.value = Automation.lanePosition(track, lane);
        }
    }

    onRevisionChanged: readValue()
    onTrackChanged: readValue()
    onLaneChanged: readValue()
    Component.onCompleted: readValue()

    // A playing lane moves its control.
    Timer {
        interval: 50
        repeat: true
        running: laneHeader.following && Project.playing && laneHeader.visible
        onTriggered: laneHeader.readValue()
    }
    // So does moving the playhead while stopped.
    Connections {
        target: Project
        enabled: laneHeader.following && !Project.playing
        function onPositionTextChanged() {
            laneHeader.readValue();
        }
        function onTimeTextChanged() {
            laneHeader.readValue();
        }
    }

    // The value with its unit, for the tooltip and screen readers.
    readonly property string valueText: {
        switch (kind) {
        case 0: // volume
        case 2: // send
            return ValueText.level(value);
        case 1:
            return ValueText.pan(value);
        case 3:
            return ValueText.param(value, paramControl, paramUnit, paramChoices);
        }
        return "";
    }

    // Closes the lane's menus, if open.
    function dismiss(): void {
        addMenu.dismiss();
        laneMenu.dismiss();
    }

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

        // The control itself, like the fader, pan knob or parameter
        // slider elsewhere. It follows the lane while the lane plays.
        QQC2.Slider {
            id: levelSlider
            visible: !laneHeader.placeholder
            Layout.preferredWidth: Kirigami.Units.gridUnit * 5
            Layout.alignment: Qt.AlignVCenter
            from: 0
            to: 1
            enabled: !laneHeader.following || laneHeader.writes
            // A press and its release are one gesture, and one undo step;
            // a key press is a gesture of its own.
            function move(): void {
                Automation.setLanePosition(laneHeader.track, laneHeader.lane, value);
                laneHeader.value = Automation.laneValue(laneHeader.track, laneHeader.lane);
            }
            onMoved: {
                if (pressed) {
                    move();
                    return;
                }
                Automation.beginLaneControl(laneHeader.track, laneHeader.lane);
                move();
                Automation.endLaneControl(laneHeader.track, laneHeader.lane);
            }
            onPressedChanged: {
                if (pressed) {
                    Automation.beginLaneControl(laneHeader.track, laneHeader.lane);
                } else {
                    Automation.endLaneControl(laneHeader.track, laneHeader.lane);
                }
            }
            Accessible.name: laneHeader.laneText
            Accessible.description: laneHeader.valueText
            // Disabled controls get no hover, so only while it can move.
            QQC2.ToolTip.text: laneHeader.valueText
            QQC2.ToolTip.visible: hovered || pressed
            QQC2.ToolTip.delay: pressed ? 0 : Kirigami.Units.toolTipDelay
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

    LazyMenu {
        id: addMenu
        menu: AddLaneMenu {
            track: laneHeader.track
        }
    }

    LazyMenu {
        id: laneMenu
        menu: QQC2.Menu {
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
    }

    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: laneMenu.popup()
    }
}
