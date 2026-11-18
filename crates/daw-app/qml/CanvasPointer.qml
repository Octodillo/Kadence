// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

// The pointer and keyboard focus of a painted editor (the timeline, the
// piano roll): lay it over the `canvas` it serves. A mouse or pen edits. A
// finger scrolls where `canvas.touchPans` says the space is empty and edits
// elsewhere; a tap there is a click, holding still asks for the context
// menu as a right click does, and a second finger can take the touch over
// for a pinch. Menu or Shift+F10 emits `menuKeyPressed`.
//
// The canvas provides press, motion, release, hover, cancelDrag, wheel,
// touchPans and cursorShape. Users add what differs: the wheel, double
// clicks and their own keys.
MouseArea {
    id: pointer

    required property Item canvas
    signal menuKeyPressed()

    acceptedButtons: Qt.LeftButton | Qt.RightButton | Qt.MiddleButton
    hoverEnabled: true
    preventStealing: true
    cursorShape: canvas.cursorShape
    activeFocusOnTab: true

    // A finger is scrolling the view.
    property bool panning: false
    // Whether the finger has moved further than a tap.
    property bool moved: false
    property point last
    property point start

    function holdMenu(): void {
        if (panning) {
            panning = false;
        } else {
            canvas.cancelDrag();
        }
        canvas.press(start.x, start.y, Qt.RightButton, 0);
        canvas.release(start.x, start.y, 0);
    }

    onPressed: (m) => {
        forceActiveFocus();
        start = Qt.point(m.x, m.y);
        last = start;
        moved = false;
        // A second finger must be able to take the touch over for the
        // pinch; a mouse drag keeps its grab.
        preventStealing = !touchPoint.active;
        if (touchPoint.active) {
            hold.restart();
            if (m.button === Qt.LeftButton && canvas.touchPans(m.x, m.y)) {
                panning = true;
                return;
            }
        }
        canvas.press(m.x, m.y, m.button, m.modifiers);
    }
    onPositionChanged: (m) => {
        if (!pressed) {
            canvas.hover(m.x, m.y);
            return;
        }
        if (!moved && Math.hypot(m.x - start.x, m.y - start.y) > Qt.styleHints.startDragDistance) {
            moved = true;
            hold.stop();
        }
        if (panning) {
            // Follow the finger the touch began with: while a second one
            // lands, the mouse position Qt makes from the touch can jump
            // between them. Once that finger lifts, stop: the other one is
            // somewhere else entirely.
            if (touchPoint.active) {
                const at = touchPoint.point.position;
                canvas.wheel(at.x, at.y, at.x - last.x, at.y - last.y, 0);
                last = at;
            }
        } else {
            canvas.motion(m.x, m.y, m.modifiers);
        }
    }
    onReleased: (m) => {
        hold.stop();
        if (panning) {
            panning = false;
            if (!moved) {
                // A tap on empty space: select or deselect as a click
                // would.
                canvas.press(start.x, start.y, Qt.LeftButton, 0);
                canvas.release(start.x, start.y, 0);
            }
        } else {
            canvas.release(m.x, m.y, m.modifiers);
        }
        // The drag may have ended over something else, or off the canvas
        // (no exit comes after a release).
        if (containsMouse) {
            canvas.hover(m.x, m.y);
        } else {
            canvas.hover(-1, -1);
        }
    }
    // Off the canvas: nothing is hovered.
    onExited: if (!pressed) canvas.hover(-1, -1)
    onCanceled: {
        hold.stop();
        panning = false;
        canvas.cancelDrag();
    }

    // Whether the press under way is a finger. Watches only: the MouseArea
    // still gets the press, as a mouse event.
    PointHandler {
        id: touchPoint
        acceptedDevices: PointerDevice.TouchScreen
    }

    // A long press with a finger opens the context menu.
    Timer {
        id: hold
        interval: Qt.styleHints.mousePressAndHoldInterval
        onTriggered: pointer.holdMenu()
    }

    Keys.onEscapePressed: canvas.cancelDrag()
    Keys.forwardTo: [menuKey]
    MenuKey {
        id: menuKey
        onRequested: pointer.menuKeyPressed()
    }
}
