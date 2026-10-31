// SPDX-License-Identifier: GPL-3.0-or-later

import QtQml
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// One row of the track list beside the timeline: colour, drag handle,
// name, instrument (on instrument tracks), MIDI input, record arm, audio
// input, mute, solo, and the button that shows the track's automation
// lanes, whose headers follow under it. Everything is read from Project and
// Automation by index; `revision` re-reads it after any change. The
// bindings use `revision` in an expression: a bare `revision;` statement is
// dropped by the QML compiler, and then the binding never re-reads.
// Dragging the handle reorders tracks; the list view shows where the track
// will land (`dragFrom`, `dragTo`).
Column {
    id: row

    required property int index
    // The timeline, for row geometry.
    required property ArrangeView arrange
    readonly property int revision: Project.revision + Automation.revision

    readonly property bool expanded: revision >= 0 ? Automation.expanded(index) : false
    readonly property int laneCount: revision >= 0 ? Automation.laneCount(index) : 0

    // The list reuses headers that scroll out of view for other tracks. A
    // hidden item keeps its focus and open menus, so close them, or the
    // next track shown here would be renamed, moved or deleted instead.
    ListView.onPooled: {
        nameField.focus = false;
        nameField.visible = false;
        menu.dismiss();
        inputMenu.dismiss();
        instrumentButton.dismiss();
        midiInputButton.dismiss();
        for (let i = 0; i < lanes.count; ++i) {
            lanes.itemAt(i).dismiss();
        }
    }

    QQC2.ItemDelegate {
        id: header

        readonly property int index: row.index
        readonly property int revision: row.revision
        width: row.width
        height: row.arrange.trackHeight

        readonly property int kind: revision >= 0 ? Project.trackKind(index) : -1
        readonly property string trackName: revision >= 0 ? Project.trackName(index) : ""
        readonly property bool muted: revision >= 0 ? Project.trackMuted(index) : false
        readonly property bool soloed: revision >= 0 ? Project.trackSoloed(index) : false
        readonly property color trackColor: revision >= 0 ? Project.trackColor(index) : "transparent"
        readonly property string instrument: revision >= 0 && kind === 1 ? Project.trackInstrument(index) : ""
        readonly property bool armed: revision >= 0 ? Project.trackArmed(index) : false
        readonly property int input: revision >= 0 ? Project.trackInput(index) : 0
        readonly property bool inputStereo: revision >= 0 ? Project.trackInputStereo(index) : false
        readonly property bool monitorAuto: revision >= 0 ? Project.trackMonitorAuto(index) : false
        readonly property int inputCount: revision >= 0 ? Project.inputCount() : 0
        // Audio (0) and instrument (1) tracks record.
        readonly property bool canRecord: kind === 0 || kind === 1
        // Inputs are counted from 1 for people.
        readonly property string inputText: inputStereo
            ? i18nc("@item a pair of audio inputs, %1 and %2 are input numbers", "Inputs %1–%2", input + 1, input + 2)
            : i18nc("@item one audio input, %1 is its number", "Input %1", input + 1)

        highlighted: Project.selectedTrack === index
        opacity: row.ListView.view && row.ListView.view.dragFrom === index ? 0.5 : 1
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

            QQC2.ToolButton {
                Layout.alignment: Qt.AlignVCenter
                icon.name: row.expanded ? "arrow-down" : "arrow-right"
                display: QQC2.AbstractButton.IconOnly
                text: row.expanded ? i18nc("@action:button", "Hide Automation") : i18nc("@action:button", "Show Automation")
                onClicked: Automation.setExpanded(header.index, !row.expanded)
                QQC2.ToolTip.text: text
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }

            // Drag to reorder.
            Kirigami.Icon {
                id: handle
                Layout.preferredWidth: Kirigami.Units.iconSizes.small
                Layout.preferredHeight: Kirigami.Units.iconSizes.small
                Layout.alignment: Qt.AlignVCenter
                source: "handle-sort"
                isMask: true
                color: Kirigami.Theme.textColor
                opacity: handleHover.hovered || drag.active ? 1 : 0.5
                Accessible.name: i18nc("@info:accessible", "Drag to move the track")

                HoverHandler {
                    id: handleHover
                    cursorShape: drag.active ? Qt.ClosedHandCursor : Qt.OpenHandCursor
                }

                DragHandler {
                    id: drag
                    target: null
                    xAxis.enabled: false
                    // The row the dragged track's middle is over.
                    readonly property int to: {
                        const mid = row.y + header.height / 2 + activeTranslation.y;
                        return row.arrange.rowAtContent(mid);
                    }
                    onActiveChanged: {
                        const list = row.ListView.view;
                        if (active) {
                            Project.selectTrack(header.index);
                            list.dragFrom = header.index;
                            list.dragTo = header.index;
                        } else {
                            const from = list.dragFrom;
                            const to = list.dragTo;
                            list.dragFrom = -1;
                            list.dragTo = -1;
                            if (from >= 0 && to >= 0 && from !== to) {
                                Project.moveTrack(from, to);
                            }
                        }
                    }
                    onToChanged: if (active) row.ListView.view.dragTo = to
                }
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

            InstrumentButton {
                id: instrumentButton
                Layout.alignment: Qt.AlignVCenter
                Layout.maximumWidth: Kirigami.Units.gridUnit * 5
                visible: header.kind === 1
                instrument: header.instrument
                currentChoice: header.revision >= 0 ? Project.trackInstrumentChoice(header.index) : -1
                onChosen: (choice, revision) => Project.setTrackInstrument(header.index, choice, revision)
                onSettingsRequested: DevicePanel.open(Project.trackInstrumentDevice(header.index))
            }

            MidiInputButton {
                id: midiInputButton
                Layout.alignment: Qt.AlignVCenter
                visible: header.kind === 1
                trackIndex: header.index
            }

            // Audio and instrument tracks record. Breeze's record icon is
            // always red, so draw the light: a ring when off, filled red when
            // armed.
            QQC2.ToolButton {
                id: armButton
                Layout.alignment: Qt.AlignVCenter
                visible: header.canRecord
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

                    // Each input on its own, then the pairs 1–2, 3–4 and so on,
                    // in one model so they stay in order above the separator.
                    Instantiator {
                        model: {
                            const choices = [];
                            for (let i = 0; i < header.inputCount; ++i) {
                                choices.push({ first: i, stereo: false });
                            }
                            for (let i = 0; i + 1 < header.inputCount; i += 2) {
                                choices.push({ first: i, stereo: true });
                            }
                            return choices;
                        }
                        delegate: QQC2.MenuItem {
                            required property var modelData
                            text: modelData.stereo
                                ? i18nc("@item:inmenu a pair of audio inputs recorded as stereo, %1 and %2 are input numbers",
                                        "Inputs %1–%2 (Stereo)", modelData.first + 1, modelData.first + 2)
                                : i18nc("@item:inmenu one audio input, %1 is its number", "Input %1", modelData.first + 1)
                            checkable: true
                            checked: header.inputStereo === modelData.stereo && header.input === modelData.first
                            // Re-picking the current choice keeps it checked.
                            onTriggered: {
                                Project.setTrackInput(header.index, modelData.first, modelData.stereo);
                                checked = Qt.binding(() => header.inputStereo === modelData.stereo
                                                           && header.input === modelData.first);
                            }
                        }
                        onObjectAdded: (i, item) => inputMenu.insertItem(i, item)
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
                text: i18nc("@action:inmenu", "Move Up")
                icon.name: "go-up"
                enabled: header.index > 0
                onTriggered: Project.moveTrack(header.index, header.index - 1)
            }
            QQC2.MenuItem {
                text: i18nc("@action:inmenu", "Move Down")
                icon.name: "go-down"
                enabled: header.index < Project.trackCount - 1
                onTriggered: Project.moveTrack(header.index, header.index + 1)
            }
            QQC2.MenuSeparator {}
            AddLaneMenu {
                track: header.index
            }
            QQC2.MenuItem {
                text: row.expanded ? i18nc("@action:inmenu", "Hide Automation") : i18nc("@action:inmenu", "Show Automation")
                icon.name: row.expanded ? "arrow-down" : "arrow-right"
                onTriggered: Automation.setExpanded(header.index, !row.expanded)
            }
            QQC2.MenuSeparator {}
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

        // Drag the lower edge to make every track taller or shorter (the
        // timeline has one track height).
        ResizeHandle {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            size: row.arrange.trackHeight
            onResizing: (size) => row.arrange.resizeTracks(size)
            onResized: AppSettings.setLayoutValue("ArrangeTrackHeight", row.arrange.trackHeight)
            Accessible.ignored: true
        }
    }

    // The lanes' headers, one per lane row the timeline shows.
    Repeater {
        id: lanes
        model: row.expanded ? Math.max(1, row.laneCount) : 0
        delegate: LaneHeader {
            required property int index
            width: row.width
            height: row.arrange.laneHeight
            track: row.index
            lane: index < row.laneCount ? index : -1
            revision: row.revision

            // Drag the lower edge to resize every automation lane.
            ResizeHandle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                size: row.arrange.laneHeight
                minimum: Kirigami.Units.gridUnit * 1.5
                maximum: Kirigami.Units.gridUnit * 12
                onResizing: (size) => row.arrange.laneHeight = Math.round(size)
                onResized: AppSettings.setLayoutValue("AutomationLaneHeight", row.arrange.laneHeight)
                Accessible.ignored: true
            }
        }
    }
}
