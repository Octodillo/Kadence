// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Dialogs
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The arrange view: track headers on the left, the painted timeline on the
// right. All editing goes through ArrangeView (pointer input) and Project
// (menus and shortcuts).
Kirigami.Page {
    id: page

    property list<Kirigami.Action> transportActions
    readonly property alias view: view

    title: i18nc("@title", "Arrange")
    padding: 0

    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.inherit: false

    readonly property Kirigami.Action loopAction: Kirigami.Action {
        text: i18nc("@action:button", "Loop")
        icon.name: "media-repeat-all"
        checkable: true
        checked: Project.looping
        shortcut: "Ctrl+L"
        tooltip: i18nc("@info:tooltip", "Loop the selection, or four bars from the playhead. Shift-drag in the ruler to set the loop.")
        onTriggered: Project.toggleLoop()
    }

    ProjectMessageText {
        id: projectMessage
    }

    // Adds a track of `kind` (0 audio, 1 instrument, 2 bus) with a
    // numbered default name in the user's language.
    function addTrack(kind: int): void {
        const n = Project.nextTrackNumber(kind);
        switch (kind) {
        case 1:
            Project.addTrack(kind, i18nc("@item default track name, %1 is a number", "Instrument %1", n));
            break;
        case 2:
            Project.addTrack(kind, i18nc("@item default track name, %1 is a number", "Bus %1", n));
            break;
        default:
            Project.addTrack(kind, i18nc("@item default track name, %1 is a number", "Audio %1", n));
        }
    }

    readonly property Kirigami.Action addTrackAction: Kirigami.Action {
        text: i18nc("@action:button", "Add Track")
        icon.name: "list-add"
        displayHint: Kirigami.DisplayHint.KeepVisible
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Audio Track")
            icon.name: "audio-x-generic"
            shortcut: "Ctrl+T"
            onTriggered: page.addTrack(0)
        }
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Instrument Track")
            icon.name: "audio-midi"
            shortcut: "Ctrl+Shift+T"
            onTriggered: page.addTrack(1)
        }
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Bus")
            icon.name: "view-media-equalizer"
            onTriggered: page.addTrack(2)
        }
    }

    readonly property Kirigami.Action importAction: Kirigami.Action {
        text: i18nc("@action:button", "Import Audio…")
        icon.name: "document-import"
        shortcut: "Ctrl+I"
        enabled: page.visible
        displayHint: Kirigami.DisplayHint.AlwaysHide
        tooltip: i18nc("@info:tooltip", "Add audio files at the playhead: the first on the selected audio track, the rest on new tracks.")
        onTriggered: importDialog.open()
    }

    FileDialog {
        id: importDialog
        title: i18nc("@title:window", "Import Audio")
        fileMode: FileDialog.OpenFiles
        nameFilters: [
            i18nc("@item:inlistbox", "Audio files (*.wav *.flac *.ogg *.oga *.mp3 *.aif *.aiff)"),
            i18nc("@item:inlistbox", "All files (*)")
        ]
        onAccepted: {
            for (let i = 0; i < selectedFiles.length; ++i) {
                Project.importAudio(selectedFiles[i], i);
            }
        }
    }

    // Gated on page.visible: the Edit page uses the same shortcuts, and two
    // enabled actions on one key make Qt treat it as ambiguous.
    readonly property list<Kirigami.Action> editActions: [
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Split at Playhead")
            icon.name: "edit-cut"
            shortcut: "Ctrl+E"
            enabled: page.visible
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.splitAtPlayhead()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Duplicate")
            icon.name: "edit-duplicate"
            shortcut: "Ctrl+D"
            enabled: page.visible && Project.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.duplicateSelection()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Delete")
            icon.name: "edit-delete"
            shortcut: StandardKey.Delete
            enabled: page.visible && Project.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.deleteSelection()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Select All")
            icon.name: "edit-select-all"
            shortcut: StandardKey.SelectAll
            enabled: page.visible
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.selectAll()
        },
        Kirigami.Action {
            separator: true
            displayHint: Kirigami.DisplayHint.AlwaysHide
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Zoom In")
            icon.name: "zoom-in"
            shortcut: StandardKey.ZoomIn
            enabled: page.visible
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.zoom(1.5)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Zoom Out")
            icon.name: "zoom-out"
            shortcut: StandardKey.ZoomOut
            enabled: page.visible
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.zoom(1 / 1.5)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Zoom to Fit")
            icon.name: "zoom-fit-width"
            shortcut: "Ctrl+0"
            enabled: page.visible
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Project.zoomToFit()
        }
    ]

    // QML list literals can't spread lists, so name each entry.
    actions: [
        page.transportActions[0],
        page.loopAction,
        page.addTrackAction,
        page.importAction,
        page.editActions[0],
        page.editActions[1],
        page.editActions[2],
        page.editActions[3],
        page.editActions[4],
        page.editActions[5],
        page.editActions[6],
        page.editActions[7]
    ]

    // Redraw on any change, and every frame while the playhead moves.
    Connections {
        target: Project
        function onRevisionChanged() { view.refresh(); }
    }
    Timer {
        interval: 16
        repeat: true
        running: Project.playing && page.visible
        onTriggered: view.refresh()
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Kirigami.InlineMessage {
            id: statusMessage
            Layout.fillWidth: true
            position: Kirigami.InlineMessage.Position.Header
            type: Kirigami.MessageType.Warning
            showCloseButton: true
            // Set rather than bound: the close button assigns visible, which
            // would break a binding and keep later messages hidden.
            visible: false
            text: projectMessage.text(Project.status)
            // Only the close button clears it, not leaving the page.
            onVisibleChanged: if (!visible && page.visible) Project.clearStatus()
            Component.onCompleted: visible = projectMessage.text(Project.status) !== ""

            Connections {
                target: Project
                function onStatusChanged() {
                    statusMessage.visible = projectMessage.text(Project.status) !== ""
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 0

            // Track headers, scrolled with the timeline.
            ColumnLayout {
                Layout.fillHeight: true
                // A child that fills makes a layout fill too; pin the width.
                Layout.fillWidth: false
                Layout.preferredWidth: Kirigami.Units.gridUnit * 19
                Layout.maximumWidth: Kirigami.Units.gridUnit * 19
                spacing: 0

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: view.rulerHeight
                    color: Kirigami.Theme.alternateBackgroundColor

                    QQC2.Label {
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.left: parent.left
                        anchors.leftMargin: Kirigami.Units.largeSpacing
                        text: Project.positionText
                        font: Kirigami.Theme.smallFont
                        Accessible.name: i18nc("@info:accessible", "Playhead position")
                    }
                    QQC2.Label {
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.right: parent.right
                        anchors.rightMargin: Kirigami.Units.largeSpacing
                        text: Project.timeText
                        font: Kirigami.Theme.smallFont
                        opacity: 0.7
                    }
                    Kirigami.Separator {
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.bottom: parent.bottom
                    }
                }

                ListView {
                    id: headers
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    interactive: false
                    model: Project.trackCount
                    contentY: view.scrollY
                    // The track whose handle is being dragged, and the row
                    // it would land on (see TrackHeader).
                    property int dragFrom: -1
                    property int dragTo: -1
                    delegate: TrackHeader {
                        width: headers.width
                        height: view.trackHeight
                    }

                    // Where a dragged track will land.
                    Rectangle {
                        visible: headers.dragFrom >= 0 && headers.dragTo !== headers.dragFrom
                        z: 1
                        x: 0
                        width: headers.width
                        height: 2
                        color: Kirigami.Theme.highlightColor
                        y: (headers.dragTo > headers.dragFrom ? headers.dragTo + 1 : headers.dragTo) * view.trackHeight - 1
                    }

                    WheelHandler {
                        onWheel: (event) => page.forwardWheel(event, 0, event.y + view.rulerHeight)
                    }
                }
            }

            Kirigami.Separator {
                Layout.fillHeight: true
            }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                ArrangeView {
                    id: view
                    anchors {
                        left: parent.left
                        top: parent.top
                        right: vbar.left
                        bottom: hbar.top
                    }
                    rulerHeight: Math.round(Kirigami.Units.gridUnit * 1.4)
                    backgroundColor: Kirigami.Theme.backgroundColor
                    alternateBackgroundColor: Kirigami.Theme.alternateBackgroundColor
                    textColor: Kirigami.Theme.textColor
                    disabledTextColor: Kirigami.Theme.disabledTextColor
                    highlightColor: Kirigami.Theme.highlightColor
                    highlightedTextColor: Kirigami.Theme.highlightedTextColor
                    negativeColor: Kirigami.Theme.negativeTextColor

                    onWidthChanged: refresh()
                    onHeightChanged: refresh()
                    onBackgroundColorChanged: refresh()
                    onTextColorChanged: refresh()
                    onHighlightColorChanged: refresh()
                    Component.onCompleted: {
                        setDefaultNames(
                            i18nc("@label default name of a new MIDI clip", "MIDI clip"),
                            i18nc("@label default name of audio from a file without a name", "Audio"),
                            i18nc("@label default name of a recorded MIDI clip", "Recording"));
                        refresh();
                    }

                    onContextMenuRequested: (x, y, onClip) => {
                        (onClip ? clipMenu : laneMenu).popup(view, x, y);
                    }

                    Accessible.role: Accessible.Canvas
                    Accessible.name: i18nc("@info:accessible", "Timeline")
                }

                MouseArea {
                    id: pointer
                    anchors.fill: view
                    acceptedButtons: Qt.LeftButton | Qt.RightButton | Qt.MiddleButton
                    hoverEnabled: true
                    preventStealing: true
                    cursorShape: view.cursorShape
                    activeFocusOnTab: true

                    onPressed: (m) => {
                        forceActiveFocus();
                        view.press(m.x, m.y, m.button, m.modifiers);
                    }
                    onPositionChanged: (m) => {
                        if (pressed) {
                            view.motion(m.x, m.y, m.modifiers);
                        } else {
                            view.hover(m.x, m.y);
                        }
                    }
                    onReleased: (m) => view.release(m.x, m.y, m.modifiers)
                    onDoubleClicked: (m) => view.doubleClick(m.x, m.y, m.modifiers)
                    onCanceled: view.cancelDrag()
                    onWheel: (w) => page.forwardWheel(w, w.x, w.y)

                    Keys.onEscapePressed: view.cancelDrag()
                    Keys.onLeftPressed: Project.nudge(-1)
                    Keys.onRightPressed: Project.nudge(1)
                }

                // The clip gain while its handle is dragged.
                QQC2.ToolTip {
                    parent: pointer
                    visible: view.gainDragging && pointer.pressed
                    x: pointer.mouseX + Kirigami.Units.gridUnit
                    y: pointer.mouseY - height - Kirigami.Units.smallSpacing
                    text: i18nc("@info:tooltip clip gain in decibels", "%1 dB",
                                (view.draggedGain > 0 ? "+" : "") + view.draggedGain.toLocaleString(Qt.locale(), "f", 1))
                }

                // Two-finger zoom on touch screens.
                PinchHandler {
                    target: null
                    property real last: 1
                    onActiveChanged: last = 1
                    onActiveScaleChanged: {
                        view.pinch(activeScale / last, centroid.position.x);
                        last = activeScale;
                    }
                }

                // Audio files dragged in from Dolphin (or any file manager).
                DropArea {
                    anchors.fill: view
                    keys: ["text/uri-list"]
                    onEntered: (drag) => drag.accept(Qt.CopyAction)
                    onDropped: (drop) => {
                        for (let i = 0; i < drop.urls.length; ++i) {
                            // The first file lands where it was dropped; the
                            // rest each get a new track.
                            const y = i === 0 ? drop.y : 1e9;
                            view.dropFile(drop.urls[i], drop.x, y);
                        }
                        drop.accept(Qt.CopyAction);
                    }
                }

                Kirigami.PlaceholderMessage {
                    anchors.centerIn: view
                    width: view.width - Kirigami.Units.gridUnit * 4
                    visible: Project.trackCount === 0
                    icon.name: "view-media-track"
                    text: i18nc("@info:placeholder", "No tracks yet")
                    explanation: i18nc("@info:placeholder", "Add a track, import audio, or drop audio files here.")
                    helpfulAction: Kirigami.Action {
                        text: i18nc("@action:button", "Add Audio Track")
                        icon.name: "list-add"
                        onTriggered: page.addTrack(0)
                    }
                }

                QQC2.ScrollBar {
                    id: hbar
                    anchors {
                        left: parent.left
                        right: vbar.left
                        bottom: parent.bottom
                    }
                    orientation: Qt.Horizontal
                    policy: QQC2.ScrollBar.AlwaysOn
                    size: view.contentWidth > 0 ? Math.min(1, view.width / view.contentWidth) : 1
                    position: view.contentWidth > 0 ? view.scrollX / view.contentWidth : 0
                    onPositionChanged: if (pressed) view.scrollTo(position * view.contentWidth, view.scrollY)
                }

                QQC2.ScrollBar {
                    id: vbar
                    anchors {
                        top: parent.top
                        right: parent.right
                        bottom: hbar.top
                        topMargin: view.rulerHeight
                    }
                    orientation: Qt.Vertical
                    policy: QQC2.ScrollBar.AsNeeded
                    readonly property real lanes: Math.max(1, view.height - view.rulerHeight)
                    size: view.contentHeight > lanes ? lanes / view.contentHeight : 1
                    position: view.contentHeight > 0 ? view.scrollY / view.contentHeight : 0
                    onPositionChanged: if (pressed) view.scrollTo(view.scrollX, position * view.contentHeight)
                }
            }
        }
    }

    function forwardWheel(event, x, y): void {
        // Mice report angles (120 per notch), touchpads pixels.
        const pixels = event.pixelDelta.x !== 0 || event.pixelDelta.y !== 0;
        const dx = pixels ? event.pixelDelta.x : event.angleDelta.x / 2;
        const dy = pixels ? event.pixelDelta.y : event.angleDelta.y / 2;
        view.wheel(x, y, dx, dy, event.modifiers);
        event.accepted = true;
    }

    QQC2.Menu {
        id: clipMenu
        QQC2.MenuItem { action: page.editActions[0] }
        QQC2.MenuItem { action: page.editActions[1] }
        QQC2.MenuItem { action: page.editActions[2] }
        QQC2.MenuSeparator { visible: Project.audioSelected }
        QQC2.Menu {
            title: i18nc("@title:menu", "Fade Shape")
            enabled: Project.audioSelected
            QQC2.MenuItem {
                text: i18nc("@action:inmenu fade shape", "Linear")
                onTriggered: Project.setFadeShape(0)
            }
            QQC2.MenuItem {
                text: i18nc("@action:inmenu fade shape", "Equal Power")
                onTriggered: Project.setFadeShape(1)
            }
            QQC2.MenuItem {
                text: i18nc("@action:inmenu fade shape", "S-Curve")
                onTriggered: Project.setFadeShape(2)
            }
        }
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Remove Fades")
            enabled: Project.audioSelected
            onTriggered: Project.removeFades()
        }
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Reset Clip Gain")
            icon.name: "edit-reset"
            enabled: Project.audioSelected
            onTriggered: Project.resetClipGain()
        }
    }

    QQC2.Menu {
        id: laneMenu
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Add Audio Track")
            icon.name: "audio-x-generic"
            onTriggered: page.addTrack(0)
        }
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Add Instrument Track")
            icon.name: "audio-midi"
            onTriggered: page.addTrack(1)
        }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { action: page.importAction }
    }
}
