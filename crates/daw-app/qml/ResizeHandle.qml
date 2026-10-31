// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A strip along the edge between two stacked areas that drags to resize
// one of them: a track row, an automation lane, the velocity lane. Put it
// over the edge and handle `resizing` (live) and `resized` (let go, time to
// remember the size). Works with a mouse, a pen and a finger.
Item {
    id: handle

    // The size of the area being resized, read when a drag starts.
    property real size: 0
    property real minimum: 0
    property real maximum: Infinity
    // True when the area is below the handle, so dragging up makes it
    // bigger (the velocity lane); false when it is above (a track row).
    property bool growsUp: false

    signal resizing(real size)
    signal resized()

    readonly property bool active: drag.active

    implicitHeight: Kirigami.Units.smallSpacing * 2

    property real startSize: 0

    HoverHandler {
        id: hover
        cursorShape: Qt.SizeVerCursor
    }

    DragHandler {
        id: drag
        target: null
        xAxis.enabled: false
        onActiveChanged: {
            if (active) {
                handle.startSize = handle.size;
            } else {
                handle.resized();
            }
        }
        onActiveTranslationChanged: {
            if (active) {
                const delta = handle.growsUp ? -activeTranslation.y : activeTranslation.y;
                handle.resizing(Math.max(handle.minimum, Math.min(handle.maximum, handle.startSize + delta)));
            }
        }
    }

    // Shows where to grab, and the edge while it moves.
    Rectangle {
        anchors.verticalCenter: parent.verticalCenter
        width: parent.width
        height: 2
        color: Kirigami.Theme.highlightColor
        visible: hover.hovered || drag.active
    }
}
