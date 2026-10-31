// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// The Edit page: the piano roll for the MIDI clip opened from the arrange
// view (double-click a clip). All editing goes through PianoRoll.
Kirigami.Page {
    id: page

    property list<Kirigami.Action> transportActions

    // The velocity lane's height, dragged at its upper edge. The lane gets
    // less when the window is too short to leave the notes three rows.
    readonly property LayoutSize velocitySize: LayoutSize {
        key: "VelocityLaneHeight"
        defaultUnits: 4
        minimumUnits: 2
        maximumUnits: 20
    }
    // The clip's track's channel strip, at the right; its width and box
    // height are its own, not the Mix page's.
    readonly property LayoutSize stripWidth: LayoutSize {
        key: "EditStripWidth"
        defaultUnits: 8
        minimumUnits: 5
        maximumUnits: 20
    }
    readonly property LayoutSize stripBoxHeight: LayoutSize {
        key: "EditStripSlotsHeight"
        defaultUnits: 11
        minimumUnits: 4
        maximumUnits: 60
    }
    // Whether the strip is shown, remembered with the layout (1 or 0).
    property bool stripShown: AppSettings.layoutValue("EditStripShown", 1) > 0
    readonly property alias roll: roll
    readonly property alias trackStrip: trackStrip

    title: roll.hasClip ? i18nc("@title %1 is a clip name", "Edit — %1", roll.clipName)
                        : i18nc("@title", "Edit")
    padding: 0

    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.inherit: false

    readonly property list<Kirigami.Action> editActions: [
        Kirigami.Action {
            text: i18nc("@action:button", "Quantize")
            icon.name: "snap-nodes-cusp"
            shortcut: "Q"
            enabled: page.visible && roll.hasSelection
            tooltip: i18nc("@info:tooltip", "Move the selected notes onto the grid")
            onTriggered: roll.quantize(1.0, false)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Quantize Starts and Ends")
            shortcut: "Shift+Q"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.quantize(1.0, true)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Quantize 50%")
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.quantize(0.5, false)
        },
        Kirigami.Action {
            separator: true
            displayHint: Kirigami.DisplayHint.AlwaysHide
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Transpose Up")
            icon.name: "go-up"
            shortcut: "Up"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.transpose(1)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Transpose Down")
            icon.name: "go-down"
            shortcut: "Down"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.transpose(-1)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Octave Up")
            shortcut: "Shift+Up"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.transpose(12)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Octave Down")
            shortcut: "Shift+Down"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.transpose(-12)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Legato")
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.legato()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Glue")
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.glue()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Fix Overlaps")
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.fixOverlaps()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Humanize")
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.humanize()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Reverse")
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.reverse()
        },
        Kirigami.Action {
            separator: true
            displayHint: Kirigami.DisplayHint.AlwaysHide
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Split at Playhead")
            icon.name: "edit-cut"
            shortcut: "Ctrl+E"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.splitAtPlayhead()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Duplicate")
            icon.name: "edit-duplicate"
            shortcut: "Ctrl+D"
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.duplicate()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Delete")
            icon.name: "edit-delete"
            shortcut: StandardKey.Delete
            enabled: page.visible && roll.hasSelection
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.deleteSelection()
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Select All")
            icon.name: "edit-select-all"
            shortcut: StandardKey.SelectAll
            enabled: page.visible && roll.hasClip
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.selectAll()
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
            onTriggered: roll.zoom(1.5)
        },
        Kirigami.Action {
            text: i18nc("@action:inmenu", "Zoom Out")
            icon.name: "zoom-out"
            shortcut: StandardKey.ZoomOut
            enabled: page.visible
            displayHint: Kirigami.DisplayHint.AlwaysHide
            onTriggered: roll.zoom(1 / 1.5)
        },
        Kirigami.Action {
            text: i18nc("@action:button", "Track Strip")
            icon.name: "view-media-equalizer"
            checkable: true
            checked: page.stripShown
            enabled: page.visible
            tooltip: i18nc("@info:tooltip", "Show the mixer strip of the clip's track beside the notes")
            onToggled: {
                page.stripShown = checked;
                AppSettings.setLayoutValue("EditStripShown", checked ? 1 : 0);
                checked = Qt.binding(() => page.stripShown);
            }
        },
        Kirigami.Action {
            text: i18nc("@action:button", "Preview Notes")
            icon.name: "audio-volume-high"
            checkable: true
            checked: AppSettings.previewNotes
            enabled: page.visible
            tooltip: i18nc("@info:tooltip", "Play keys and notes through the track's instrument as you press or move them")
            onToggled: {
                AppSettings.choosePreviewNotes(checked);
                // Toggling replaced the binding; follow the setting again.
                checked = Qt.binding(() => AppSettings.previewNotes);
            }
        }
    ]

    // QML list literals can't spread lists, so name each entry.
    actions: [
        page.transportActions[0],
        page.editActions[0],
        page.editActions[1],
        page.editActions[2],
        page.editActions[3],
        page.editActions[4],
        page.editActions[5],
        page.editActions[6],
        page.editActions[7],
        page.editActions[8],
        page.editActions[9],
        page.editActions[10],
        page.editActions[11],
        page.editActions[12],
        page.editActions[13],
        page.editActions[14],
        page.editActions[15],
        page.editActions[16],
        page.editActions[17],
        page.editActions[18],
        page.editActions[19],
        page.editActions[20],
        page.editActions[21],
        page.editActions[22]
    ]

    // The strip's row follows the project, so the Mixer's rows must too
    // before the roll says which row is the clip's track.
    function refresh(): void {
        Mixer.sync();
        roll.refresh();
    }
    Connections {
        target: Project
        function onRevisionChanged() { page.refresh(); }
    }
    Timer {
        interval: 16
        repeat: true
        running: Project.playing && page.visible
        onTriggered: roll.refresh()
    }
    onVisibleChanged: if (visible) page.refresh()
    MeterClock {
        running: page.visible && trackStrip.shown
    }

    TrackStrip {
        id: trackStrip
        anchors {
            top: parent.top
            right: parent.right
            bottom: parent.bottom
        }
        visible: page.stripShown
        row: page.stripShown ? roll.stripRow : -1
        widthSize: page.stripWidth
        boxSize: page.stripBoxHeight
    }

    Item {
        anchors {
            left: parent.left
            top: parent.top
            right: trackStrip.left
            bottom: parent.bottom
        }

        PianoRoll {
            id: roll
            anchors {
                left: parent.left
                top: parent.top
                right: vbar.left
                bottom: hbar.top
            }
            rulerHeight: Math.round(Kirigami.Units.gridUnit * 1.4)
            keyboardWidth: Kirigami.Units.gridUnit * 3
            readonly property real maximumVelocityHeight: Math.max(page.velocitySize.minimumPixels, height - rulerHeight - Kirigami.Units.gridUnit * 3)
            velocityHeight: Math.round(Math.min(page.velocitySize.pixels, maximumVelocityHeight))
            backgroundColor: Kirigami.Theme.backgroundColor
            alternateBackgroundColor: Kirigami.Theme.alternateBackgroundColor
            textColor: Kirigami.Theme.textColor
            disabledTextColor: Kirigami.Theme.disabledTextColor
            highlightColor: Kirigami.Theme.highlightColor
            highlightedTextColor: Kirigami.Theme.highlightedTextColor
            negativeColor: Kirigami.Theme.negativeTextColor

            onWidthChanged: refresh()
            onHeightChanged: refresh()
            onVelocityHeightChanged: refresh()
            onBackgroundColorChanged: refresh()
            onTextColorChanged: refresh()
            onHighlightColorChanged: refresh()
            Component.onCompleted: refresh()

            onContextMenuRequested: (x, y) => noteMenu.popup(roll, x, y)

            Accessible.role: Accessible.Canvas
            Accessible.name: i18nc("@info:accessible", "Piano roll")
        }

        MouseArea {
            anchors.fill: roll
            enabled: page.visible && roll.hasClip
            acceptedButtons: Qt.LeftButton | Qt.RightButton | Qt.MiddleButton
            hoverEnabled: true
            preventStealing: true
            cursorShape: roll.cursorShape
            activeFocusOnTab: true

            onPressed: (m) => {
                forceActiveFocus();
                roll.press(m.x, m.y, m.button, m.modifiers);
            }
            onPositionChanged: (m) => {
                if (pressed) {
                    roll.motion(m.x, m.y, m.modifiers);
                } else {
                    roll.hover(m.x, m.y);
                }
            }
            onReleased: (m) => roll.release(m.x, m.y, m.modifiers)
            onDoubleClicked: (m) => roll.doubleClick(m.x, m.y, m.modifiers)
            onCanceled: roll.cancelDrag()
            onWheel: (w) => {
                // Mice report angles (120 per notch), touchpads pixels.
                const pixels = w.pixelDelta.x !== 0 || w.pixelDelta.y !== 0;
                const dx = pixels ? w.pixelDelta.x : w.angleDelta.x / 2;
                const dy = pixels ? w.pixelDelta.y : w.angleDelta.y / 2;
                roll.wheel(w.x, w.y, dx, dy, w.modifiers);
                w.accepted = true;
            }

            Keys.onEscapePressed: roll.cancelDrag()
            Keys.onLeftPressed: (e) => roll.nudge(-1)
            Keys.onRightPressed: (e) => roll.nudge(1)
        }

        // Between the notes and the velocity lane: drag it to resize the
        // lane (up is taller), double-click for the default.
        SplitHandle {
            visible: roll.hasClip
            enabled: page.visible
            // The roll draws the lane's edge itself.
            lineVisible: false
            x: roll.x
            width: roll.width
            y: roll.y + roll.height - roll.velocityHeight - height / 2
            onStarted: page.velocitySize.begin(roll.velocityHeight, roll.maximumVelocityHeight)
            onMoved: (delta, done) => page.velocitySize.drag(-delta, done)
            onCanceled: page.velocitySize.cancel()
            onReset: page.velocitySize.reset()
            Accessible.ignored: true
        }

        // Two-finger zoom on touch screens.
        PinchHandler {
            target: null
            enabled: page.visible && roll.hasClip
            property real last: 1
            onActiveChanged: last = 1
            onActiveScaleChanged: {
                roll.pinch(activeScale / last, centroid.position.x);
                last = activeScale;
            }
        }

        Kirigami.PlaceholderMessage {
            anchors.centerIn: roll
            width: roll.width - Kirigami.Units.gridUnit * 4
            visible: !roll.hasClip
            icon.name: "audio-midi"
            text: i18nc("@info:placeholder", "No clip selected")
            explanation: i18nc("@info:placeholder", "Double-click a MIDI clip in the Arrange view to edit its notes here.")
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
            visible: roll.hasClip
            // Scroll a generous 64 bars past the left edge.
            readonly property real span: Math.max(roll.width, roll.scrollX + roll.width * 4)
            size: Math.min(1, roll.width / span)
            position: roll.scrollX / span
            onPositionChanged: if (pressed) roll.scrollTo(position * span, roll.scrollY)
        }

        QQC2.ScrollBar {
            id: vbar
            anchors {
                top: parent.top
                right: parent.right
                bottom: hbar.top
                topMargin: roll.rulerHeight
                bottomMargin: roll.velocityHeight
            }
            orientation: Qt.Vertical
            policy: QQC2.ScrollBar.AsNeeded
            visible: roll.hasClip
            readonly property real rows: Math.max(1, roll.height - roll.rulerHeight - roll.velocityHeight)
            size: roll.contentHeight > rows ? rows / roll.contentHeight : 1
            position: roll.contentHeight > 0 ? roll.scrollY / roll.contentHeight : 0
            onPositionChanged: if (pressed) roll.scrollTo(roll.scrollX, position * roll.contentHeight)
        }
    }

    QQC2.Menu {
        id: noteMenu
        QQC2.MenuItem { action: page.editActions[0] }
        QQC2.MenuItem { action: page.editActions[4] }
        QQC2.MenuItem { action: page.editActions[5] }
        QQC2.MenuItem { action: page.editActions[8] }
        QQC2.MenuSeparator {}
        QQC2.MenuItem { action: page.editActions[16] }
        QQC2.MenuItem { action: page.editActions[17] }
    }
}
