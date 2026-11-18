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

    // Touch drags on the timeline pan and edit; the page row must not take
    // them for swiping between pages (the timeline lets touches be stolen
    // so a pinch can take over).
    Kirigami.ColumnView.preventStealing: true

    property list<Kirigami.Action> transportActions
    readonly property alias view: view
    readonly property alias transportBar: transportBar
    readonly property alias trackHeaders: headers

    // The automation lanes' height, dragged at a lane header's lower edge.
    readonly property LayoutSize laneSize: LayoutSize {
        key: "AutomationLaneHeight"
        defaultUnits: 2.2
        minimumUnits: 1.5
        maximumUnits: 12
    }
    // The tracks' height in grid units, as last left (by a header's edge
    // or zooming); 0 for the timeline's own default.
    readonly property string trackHeightKey: "ArrangeTrackHeight"

    // A project in the welcome list was clicked: open Project.recentProjects[index].
    signal recentActivated(int index)

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

    // Adds a track of `kind` (a Tracks.TrackKind) with a numbered default
    // name in the user's language.
    function addTrack(kind: int): void {
        const n = Tracks.nextNumber(kind);
        switch (kind) {
        case Tracks.InstrumentTrack:
            Tracks.add(kind, i18nc("@item default track name, %1 is a number", "Instrument %1", n));
            break;
        case Tracks.BusTrack:
            Tracks.add(kind, i18nc("@item default track name, %1 is a number", "Bus %1", n));
            break;
        default:
            Tracks.add(kind, i18nc("@item default track name, %1 is a number", "Audio %1", n));
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
            onTriggered: page.addTrack(Tracks.AudioTrack)
        }
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Instrument Track")
            icon.name: "audio-midi"
            shortcut: "Ctrl+Shift+T"
            onTriggered: page.addTrack(Tracks.InstrumentTrack)
        }
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Bus")
            icon.name: "view-media-equalizer"
            onTriggered: page.addTrack(Tracks.BusTrack)
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

    // What a track header's context menu does, for the selected track: the
    // keyboard's way to it, whatever has the focus.
    readonly property bool trackSelected: Project.selectedTrack >= 0 && Project.selectedTrack < Tracks.count
    readonly property list<Kirigami.Action> trackActions: [
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Rename Track…")
            icon.name: "edit-rename"
            shortcut: "F2"
            enabled: page.visible && page.trackSelected
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: {
                // The header may be scrolled out of view, where a list has
                // no item for it.
                const row = Project.selectedTrack;
                page.revealTrack(row);
                headers.forceLayout();
                headers.itemAtIndex(row)?.rename();
            }
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Move Track Up")
            icon.name: "go-up"
            shortcut: "Alt+Up"
            enabled: page.visible && page.trackSelected && Project.selectedTrack > 0
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Tracks.moveTrack(Project.selectedTrack, Project.selectedTrack - 1)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Move Track Down")
            icon.name: "go-down"
            shortcut: "Alt+Down"
            enabled: page.visible && page.trackSelected && Project.selectedTrack < Tracks.count - 1
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Tracks.moveTrack(Project.selectedTrack, Project.selectedTrack + 1)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Delete Track")
            icon.name: "edit-delete"
            enabled: page.visible && page.trackSelected
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: Tracks.remove(Project.selectedTrack)
        },
        Kirigami.Action {
            separator: true
            displayHint: Kirigami.DisplayHint.AlwaysHide
        }
    ]

    // QML list literals can't spread lists, so name each entry.
    actions: [
        page.transportActions[0],
        page.transportActions[1],
        page.loopAction,
        page.addTrackAction,
        page.importAction,
        page.trackActions[0],
        page.trackActions[1],
        page.trackActions[2],
        page.trackActions[3],
        page.trackActions[4],
        page.editActions[0],
        page.editActions[1],
        page.editActions[2],
        page.editActions[3],
        page.editActions[4],
        page.editActions[5],
        page.editActions[6],
        page.editActions[7]
    ]

    // Shows a change to the project in the timeline. (The track headers
    // follow by themselves: see UiClock.) Hidden, the timeline only notes
    // that it is stale, and draws when it shows again.
    function refresh(): void {
        view.refresh();
    }

    // Redraw on any change, and every frame while the playhead moves.
    Connections {
        target: Project
        function onRevisionChanged() { page.refresh(); }
    }
    Connections {
        target: Automation
        function onRevisionChanged() { page.refresh(); }
    }
    Connections {
        target: UiClock
        enabled: page.visible
        function onFrame() { view.refresh(); }
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

        TransportBar {
            id: transportBar
            Layout.fillWidth: true
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
                Layout.preferredWidth: Kirigami.Units.gridUnit * 21
                Layout.maximumWidth: Kirigami.Units.gridUnit * 21
                spacing: 0

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: view.rulerHeight
                    color: Kirigami.Theme.alternateBackgroundColor

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
                    model: Tracks
                    contentY: view.scrollY
                    // Scrolling must not build and destroy a header per
                    // row: reuse the ones that leave the view.
                    reuseItems: true
                    // The track whose handle is being dragged, and the row
                    // it would land on (see TrackHeader).
                    property int dragFrom: -1
                    property int dragTo: -1
                    delegate: TrackHeader {
                        width: headers.width
                        arrange: view
                        laneSize: page.laneSize
                    }

                    // Where a dragged track will land.
                    Rectangle {
                        visible: headers.dragFrom >= 0 && headers.dragTo !== headers.dragFrom
                        z: 1
                        x: 0
                        width: headers.width
                        height: 2
                        color: Kirigami.Theme.highlightColor
                        y: {
                            const item = headers.itemAtIndex(headers.dragTo);
                            if (!item) {
                                return 0;
                            }
                            return (headers.dragTo > headers.dragFrom ? item.y + item.height : item.y) - 1;
                        }
                    }

                    WheelHandler {
                        onWheel: (event) => page.forwardWheel(event, 0, event.y + view.rulerHeight)
                    }

                    // A finger dragged up or down the headers scrolls the
                    // tracks, as on the timeline.
                    DragHandler {
                        target: null
                        acceptedDevices: PointerDevice.TouchScreen
                        xAxis.enabled: false
                        property real last: 0
                        onActiveChanged: last = 0
                        onActiveTranslationChanged: {
                            if (active) {
                                view.wheel(0, centroid.position.y + view.rulerHeight, 0, activeTranslation.y - last, 0);
                                last = activeTranslation.y;
                            }
                        }
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
                    laneHeight: Math.round(page.laneSize.pixels)
                    // Remember the track height however it changed, once it
                    // settles.
                    onTrackHeightChanged: saveTrackHeight.restart()
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
                        const units = AppSettings.layoutValue(page.trackHeightKey, 0);
                        if (units > 0) {
                            resizeTracks(units * Kirigami.Units.gridUnit, rulerHeight);
                            // That is the saved height already.
                            saveTrackHeight.stop();
                        }
                        setDefaultNames(
                            i18nc("@label default name of a new MIDI clip", "MIDI clip"),
                            i18nc("@label default name of audio from a file without a name", "Audio"),
                            i18nc("@label default name of a recorded MIDI clip", "Recording"));
                        refresh();
                    }

                    onContextMenuRequested: (x, y, onClip) => {
                        (onClip ? clipMenu : laneMenu).popup(view, x, y);
                    }
                    onAutomationMenuRequested: (x, y, onPoint) => {
                        pointMenu.shape = onPoint ? Automation.pointShape() : -1;
                        pointMenu.popup(view, x, y);
                    }
                    onLaneHeightChanged: refresh()

                    Accessible.role: Accessible.Canvas
                    Accessible.name: i18nc("@info:accessible", "Timeline")

                    Timer {
                        id: saveTrackHeight
                        interval: 1000
                        onTriggered: AppSettings.setLayoutValue(page.trackHeightKey, view.trackHeight / Kirigami.Units.gridUnit)
                    }
                }

                CanvasPointer {
                    id: pointer
                    anchors.fill: view
                    canvas: view

                    onDoubleClicked: (m) => view.doubleClick(m.x, m.y, m.modifiers)
                    onWheel: (w) => {
                        page.forwardWheel(w, w.x, w.y);
                        // The content moved under the pointer.
                        view.hover(w.x, w.y);
                    }
                    // What a right click would open, for the selection: no
                    // place on the timeline is the keyboard's.
                    onMenuKeyPressed: {
                        const at = Qt.point(Kirigami.Units.gridUnit, view.rulerHeight + Kirigami.Units.gridUnit);
                        const shape = Automation.pointShape();
                        if (shape >= 0) {
                            pointMenu.shape = shape;
                            pointMenu.popup(view, at.x, at.y);
                        } else {
                            (Project.hasSelection ? clipMenu : laneMenu).popup(view, at.x, at.y);
                        }
                    }

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
                    id: placeholder
                    anchors.centerIn: view
                    width: Math.min(view.width - Kirigami.Units.gridUnit * 4, Kirigami.Units.gridUnit * 24)
                    visible: Tracks.count === 0
                    icon.name: "view-media-track"
                    text: i18nc("@info:placeholder", "No tracks yet")
                    explanation: i18nc("@info:placeholder", "Add a track, import audio, or drop audio files here.")
                    helpfulAction: Kirigami.Action {
                        text: i18nc("@action:button", "Add Audio Track")
                        icon.name: "list-add"
                        onTriggered: page.addTrack(Tracks.AudioTrack)
                    }

                    // On a new, untouched project: pick up where you left off.
                    readonly property bool showRecent: !Project.savedBefore && !Project.modified
                                                       && Project.recentProjects.length > 0

                    Kirigami.Heading {
                        Layout.fillWidth: true
                        Layout.topMargin: Kirigami.Units.gridUnit
                        visible: placeholder.showRecent
                        level: 3
                        horizontalAlignment: Text.AlignHCenter
                        text: i18nc("@title", "Recent Projects")
                    }

                    Repeater {
                        model: placeholder.showRecent ? Project.recentProjects.slice(0, 5) : []
                        delegate: QQC2.ItemDelegate {
                            required property int index
                            required property string modelData
                            Layout.alignment: Qt.AlignHCenter
                            Layout.maximumWidth: placeholder.width
                            icon.name: "folder-sound"
                            text: modelData.split("/").pop()
                            QQC2.ToolTip.text: modelData
                            QQC2.ToolTip.visible: hovered
                            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                            onClicked: page.recentActivated(index)
                        }
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

    // Scrolls the track at `row` into view, if it is not.
    function revealTrack(row: int): void {
        let top = 0;
        for (let i = 0; i < row; ++i) {
            top += view.rowHeight(i);
        }
        const shown = view.height - view.rulerHeight;
        if (top < view.scrollY || top + view.trackHeight > view.scrollY + shown) {
            view.scrollTo(view.scrollX, top);
        }
    }

    function forwardWheel(event, x, y): void {
        const by = Wheel.pixels(event);
        view.wheel(x, y, by.x, by.y, event.modifiers);
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
                onTriggered: Project.setFadeShape(Project.LinearFade)
            }
            QQC2.MenuItem {
                text: i18nc("@action:inmenu fade shape", "Equal Power")
                onTriggered: Project.setFadeShape(Project.EqualPowerFade)
            }
            QQC2.MenuItem {
                text: i18nc("@action:inmenu fade shape", "S-Curve")
                onTriggered: Project.setFadeShape(Project.SCurveFade)
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

    // On an automation lane: the selected points' shape.
    PointMenu {
        id: pointMenu
        onShapeChosen: (shape) => Automation.setPointShape(shape)
        onDeleteChosen: Automation.deletePoints()
    }

    QQC2.Menu {
        id: laneMenu
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Add Audio Track")
            icon.name: "audio-x-generic"
            onTriggered: page.addTrack(Tracks.AudioTrack)
        }
        QQC2.MenuItem {
            text: i18nc("@action:inmenu", "Add Instrument Track")
            icon.name: "audio-midi"
            onTriggered: page.addTrack(Tracks.InstrumentTrack)
        }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { action: page.importAction }
    }
}
