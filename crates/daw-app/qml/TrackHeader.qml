// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// One row of the track list beside the timeline: colour, drag handle,
// name, mute, solo, and the button that shows the track's automation
// lanes, whose headers follow under it. Everything is read from Project
// and Automation by index; `revision` re-reads it after any change.
// Dragging the handle reorders tracks; the list view shows where the track
// will land (`dragFrom`, `dragTo`).
Column {
    id: row

    required property int index
    // The timeline, for row geometry.
    required property ArrangeView arrange
    readonly property int revision: Project.revision + Automation.revision

    readonly property bool expanded: { revision; return Automation.expanded(index); }
    readonly property int laneCount: { revision; return Automation.laneCount(index); }

    QQC2.ItemDelegate {
        id: header

        readonly property int index: row.index
        readonly property int revision: row.revision
        width: row.width
        height: row.arrange.trackHeight

        readonly property int kind: { revision; return Project.trackKind(index); }
        readonly property string trackName: { revision; return Project.trackName(index); }
        readonly property bool muted: { revision; return Project.trackMuted(index); }
        readonly property bool soloed: { revision; return Project.trackSoloed(index); }
        readonly property color trackColor: { revision; return Project.trackColor(index); }

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
    }

    // The lanes' headers, one per lane row the timeline shows.
    Repeater {
        model: row.expanded ? Math.max(1, row.laneCount) : 0
        delegate: LaneHeader {
            required property int index
            width: row.width
            height: row.arrange.laneHeight
            track: row.index
            lane: index < row.laneCount ? index : -1
            revision: row.revision
        }
    }
}
