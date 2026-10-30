// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// One row of the track list beside the timeline: colour, name, MIDI input,
// arm, mute, solo.
// Everything is read from Project by index; `revision` re-reads it after
// any change. The bindings use `revision` in an expression: a bare
// `revision;` statement is dropped by the QML compiler, and then the
// binding never re-reads (an undone mute or a rename stayed stale).
QQC2.ItemDelegate {
    id: header

    required property int index
    property int revision: Project.revision

    readonly property int kind: revision >= 0 ? Project.trackKind(index) : -1
    readonly property string trackName: revision >= 0 ? Project.trackName(index) : ""
    readonly property bool muted: revision >= 0 ? Project.trackMuted(index) : false
    readonly property bool soloed: revision >= 0 ? Project.trackSoloed(index) : false
    readonly property bool armed: revision >= 0 ? Project.trackArmed(index) : false
    readonly property color trackColor: revision >= 0 ? Project.trackColor(index) : "transparent"

    highlighted: Project.selectedTrack === index
    padding: 0
    leftPadding: 0
    onClicked: Project.selectTrack(index)

    Accessible.name: trackName

    contentItem: RowLayout {
        spacing: Kirigami.Units.smallSpacing

        // The track colour, as in the clips.
        Rectangle {
            Layout.fillHeight: true
            Layout.preferredWidth: Kirigami.Units.smallSpacing
            color: header.trackColor
        }

        Kirigami.Icon {
            Layout.preferredWidth: Kirigami.Units.iconSizes.small
            Layout.preferredHeight: Kirigami.Units.iconSizes.small
            Layout.alignment: Qt.AlignVCenter
            source: header.kind === 1 ? "audio-midi" : header.kind === 2 ? "view-media-equalizer" : "audio-x-generic"
            isMask: true
            color: Kirigami.Theme.textColor
        }

        QQC2.Label {
            id: nameLabel
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            text: header.trackName
            elide: Text.ElideRight
            visible: !nameField.visible

            TapHandler {
                onDoubleTapped: {
                    nameField.text = header.trackName;
                    nameField.visible = true;
                    nameField.forceActiveFocus();
                    nameField.selectAll();
                }
            }
        }

        QQC2.TextField {
            id: nameField
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            visible: false
            onAccepted: {
                Project.renameTrack(header.index, text);
                visible = false;
            }
            onActiveFocusChanged: if (!activeFocus) visible = false
            Keys.onEscapePressed: visible = false
        }

        MidiInputButton {
            Layout.alignment: Qt.AlignVCenter
            visible: header.kind === 1
            trackIndex: header.index
        }

        // Audio and instrument tracks record; buses don't.
        // Breeze's record icon is always red, so draw the light: a ring
        // when off, filled red when armed.
        QQC2.ToolButton {
            id: armButton
            Layout.alignment: Qt.AlignVCenter
            visible: header.kind === 0 || header.kind === 1
            contentItem: Item {
                implicitWidth: Kirigami.Units.iconSizes.smallMedium
                implicitHeight: Kirigami.Units.iconSizes.smallMedium
                Rectangle {
                    anchors.centerIn: parent
                    width: Math.round(parent.width * 0.6)
                    height: width
                    radius: width / 2
                    color: armButton.checked ? Kirigami.Theme.negativeTextColor : "transparent"
                    border.width: 2
                    border.color: armButton.checked ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                }
            }
            checkable: true
            checked: header.armed
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button arm track for recording", "Arm")
            onToggled: Project.setTrackArmed(header.index, checked)
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Arm for recording. An armed instrument track plays what you play live.")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        QQC2.ToolButton {
            Layout.alignment: Qt.AlignVCenter
            icon.name: "audio-volume-muted"
            checkable: true
            checked: header.muted
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button", "Mute")
            onToggled: Project.setTrackMuted(header.index, checked)
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        QQC2.ToolButton {
            Layout.alignment: Qt.AlignVCenter
            Layout.rightMargin: Kirigami.Units.smallSpacing
            icon.name: "audio-headphones"
            checkable: true
            checked: header.soloed
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button", "Solo")
            onToggled: Project.setTrackSoloed(header.index, checked)
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
    }

    // A line between rows, matching the timeline lanes.
    Kirigami.Separator {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
    }

    QQC2.Menu {
        id: menu
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Rename…")
            icon.name: "edit-rename"
            onTriggered: {
                nameField.text = header.trackName;
                nameField.visible = true;
                nameField.forceActiveFocus();
                nameField.selectAll();
            }
        }
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Delete Track")
            icon.name: "edit-delete"
            onTriggered: Project.removeTrack(header.index)
        }
    }

    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: {
            Project.selectTrack(header.index);
            menu.popup();
        }
    }
}
