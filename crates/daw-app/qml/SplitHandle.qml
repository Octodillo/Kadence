// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A line to drag to resize what is beside it; double-click it for the
// default size. Pair it with a LayoutSize:
//   SplitHandle {
//       onStarted: size.begin(size.pixels, Infinity)
//       onMoved: (delta, done) => size.drag(delta, done)
//       onCanceled: size.cancel()
//       onReset: size.reset()
//   }
Item {
    id: handle

    // Qt.Horizontal: a horizontal line, dragged up and down; Qt.Vertical:
    // a vertical line, dragged sideways.
    property int orientation: Qt.Horizontal
    // How far the line moves for each pixel the size changes: more than 1
    // when other things grow with it (strips before a strip's edge).
    property real ratio: 1
    property bool lineVisible: true

    readonly property bool sideways: orientation === Qt.Vertical

    signal started()
    // `delta`: how much bigger, in pixels, since `started`; `done` once
    // let go.
    signal moved(delta: real, done: bool)
    // The drag was taken away: go back to the size at `started`.
    signal canceled()
    signal reset()

    implicitWidth: Kirigami.Units.smallSpacing * 2
    implicitHeight: Kirigami.Units.smallSpacing * 2

    Kirigami.Separator {
        visible: handle.lineVisible
        anchors.centerIn: parent
        width: handle.sideways ? implicitWidth : parent.width
        height: handle.sideways ? parent.height : implicitHeight
    }

    // A MouseArea rather than a DragHandler: it takes the press from what
    // is under it (a view that scrolls, a full-size MouseArea) and keeps it.
    MouseArea {
        anchors.fill: parent
        cursorShape: handle.sideways ? Qt.SplitHCursor : Qt.SplitVCursor
        preventStealing: true
        // In the window, as the handle itself moves while it resizes.
        property point start
        property bool dragging: false
        // After a double-click, until let go: its reset stands.
        property bool resetting: false

        // How far the pointer moved along the drag, in the window.
        function travel(mouse: var): real {
            const p = mapToItem(null, mouse.x, mouse.y);
            // Right to left, a sideways line moves the other way.
            return !handle.sideways ? p.y - start.y
                : handle.LayoutMirroring.enabled ? start.x - p.x : p.x - start.x;
        }
        onPressed: mouse => {
            start = mapToItem(null, mouse.x, mouse.y);
            dragging = false;
        }
        onPositionChanged: mouse => {
            const t = travel(mouse);
            if (!dragging) {
                // A click that shakes a little is not a drag.
                if (resetting || Math.abs(t) < Qt.styleHints.startDragDistance) {
                    return;
                }
                dragging = true;
                handle.started();
            }
            handle.moved(t / handle.ratio, false);
        }
        onReleased: mouse => {
            if (dragging) {
                handle.moved(travel(mouse) / handle.ratio, true);
            }
            dragging = false;
            resetting = false;
        }
        onCanceled: {
            if (dragging) {
                handle.canceled();
            }
            dragging = false;
            resetting = false;
        }
        onDoubleClicked: {
            resetting = true;
            handle.reset();
        }
    }
}
