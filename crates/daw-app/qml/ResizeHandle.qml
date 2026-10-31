// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A strip along the edge between two stacked areas that drags to resize
// one of them: a track row, an automation lane, the velocity lane. Put it
// over the edge and handle `started` (the drag begins), `resizing` (live)
// and `resized` (let go, time to remember the size). Works with a mouse, a
// pen and a finger.
Item {
    id: handle

    // The size of the area being resized, read when a drag starts.
    property real size: 0
    property real minimum: 0
    property real maximum: Infinity
    // True when the area is below the handle, so dragging up makes it
    // bigger (the velocity lane); false when it is above (a track row).
    property bool growsUp: false
    // How many sizes the pointer's movement spreads over: with n lanes of
    // one height above the edge, each grows by 1/n of it, so the edge
    // stays under the pointer.
    property real divisor: 1

    signal started()
    signal resizing(real size)
    signal resized()

    readonly property bool active: area.pressed

    implicitHeight: Kirigami.Units.smallSpacing * 2

    // A MouseArea rather than a DragHandler: it takes the press, so the
    // timeline or piano roll underneath can't start an edit there.
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        preventStealing: true
        cursorShape: Qt.SizeVerCursor

        property real startY: 0
        property real startSize: 0
        property real startDivisor: 1

        onPressed: (mouse) => {
            // In scene coordinates: the handle moves as the area resizes.
            startY = mapToItem(null, mouse.x, mouse.y).y;
            startSize = handle.size;
            startDivisor = Math.max(1, handle.divisor);
            handle.started();
        }
        onPositionChanged: (mouse) => {
            if (!pressed) {
                return;
            }
            const moved = mapToItem(null, mouse.x, mouse.y).y - startY;
            const delta = (handle.growsUp ? -moved : moved) / startDivisor;
            handle.resizing(Math.max(handle.minimum, Math.min(handle.maximum, startSize + delta)));
        }
        onReleased: handle.resized()
        onCanceled: handle.resized()
    }

    // Shows where to grab, and the edge while it moves.
    Rectangle {
        anchors.verticalCenter: parent.verticalCenter
        width: parent.width
        height: 2
        color: Kirigami.Theme.highlightColor
        visible: area.containsMouse || area.pressed
    }
}
