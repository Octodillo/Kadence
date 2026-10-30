// SPDX-License-Identifier: GPL-3.0-or-later

import QtQml
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// One row of the track list beside the timeline: colour, name, record arm,
// audio input, mute, solo.
// Everything is read from Project by index; `revision` re-reads it after
// any change.
QQC2.ItemDelegate {
    id: header

    required property int index
    property int revision: Project.revision

    readonly property int kind: { revision; return Project.trackKind(index); }
    readonly property string trackName: { revision; return Project.trackName(index); }
    readonly property bool muted: { revision; return Project.trackMuted(index); }
    readonly property bool soloed: { revision; return Project.trackSoloed(index); }
    readonly property color trackColor: { revision; return Project.trackColor(index); }
    readonly property bool armed: { revision; return Project.trackArmed(index); }
    readonly property int input: { revision; return Project.trackInput(index); }
    readonly property bool inputStereo: { revision; return Project.trackInputStereo(index); }
    readonly property bool monitorAuto: { revision; return Project.trackMonitorAuto(index); }
    readonly property int inputCount: { revision; return Project.inputCount(); }
    // Audio (0) and instrument (1) tracks record.
    readonly property bool canRecord: kind === 0 || kind === 1
    // Inputs are counted from 1 for people.
    readonly property string inputText: inputStereo
        ? i18nc("@item a pair of audio inputs, %1 and %2 are input numbers", "Inputs %1–%2", input + 1, input + 2)
        : i18nc("@item one audio input, %1 is its number", "Input %1", input + 1)

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

        QQC2.ToolButton {
            Layout.alignment: Qt.AlignVCenter
            visible: header.canRecord
            icon.name: "media-record"
            checkable: true
            checked: header.armed
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button arm track for recording", "Arm")
            onToggled: Project.setTrackArmed(header.index, checked)
            QQC2.ToolTip.text: i18nc("@info:tooltip", "Arm for recording")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }

        // The audio input: which inputs to record and whether to monitor.
        QQC2.ToolButton {
            id: inputButton
            Layout.alignment: Qt.AlignVCenter
            visible: header.kind === 0
            icon.name: "audio-input-microphone"
            display: QQC2.AbstractButton.IconOnly
            text: i18nc("@action:button %1 is the chosen input, like Input 1", "Audio input: %1", header.inputText)
            down: inputMenu.visible
            onClicked: inputMenu.popup(inputButton, 0, inputButton.height)
            QQC2.ToolTip.text: text
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay

            QQC2.Menu {
                id: inputMenu

                Instantiator {
                    model: header.inputCount
                    delegate: QQC2.MenuItem {
                        required property int index
                        text: i18nc("@item:inmenu one audio input, %1 is its number", "Input %1", index + 1)
                        checkable: true
                        checked: !header.inputStereo && header.input === index
                        onTriggered: Project.setTrackInput(header.index, index, false)
                    }
                    onObjectAdded: (i, item) => inputMenu.insertItem(i, item)
                    onObjectRemoved: (i, item) => inputMenu.removeItem(item)
                }

                // Pairs 1–2, 3–4 and so on.
                Instantiator {
                    model: Math.floor(header.inputCount / 2)
                    delegate: QQC2.MenuItem {
                        required property int index
                        text: i18nc("@item:inmenu a pair of audio inputs recorded as stereo, %1 and %2 are input numbers",
                                    "Inputs %1–%2 (Stereo)", 2 * index + 1, 2 * index + 2)
                        checkable: true
                        checked: header.inputStereo && header.input === 2 * index
                        onTriggered: Project.setTrackInput(header.index, 2 * index, true)
                    }
                    onObjectAdded: (i, item) => inputMenu.insertItem(header.inputCount + i, item)
                    onObjectRemoved: (i, item) => inputMenu.removeItem(item)
                }

                QQC2.MenuSeparator {}

                QQC2.MenuItem {
                    text: i18nc("@option:check", "Monitor Input While Armed")
                    checkable: true
                    checked: header.monitorAuto
                    onTriggered: Project.setTrackMonitorAuto(header.index, checked)
                }
            }
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
