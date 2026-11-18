// SPDX-License-Identifier: GPL-3.0-or-later

pragma Singleton

import QtQuick

// Reads wheel events for the views that scroll and zoom themselves (the
// timeline and the piano roll): Wheel.pixels(event).
QtObject {
    // How far `event` (a WheelEvent) scrolls, in pixels along x and y.
    // Mice report angles (120 per notch), touchpads pixels.
    function pixels(event): point {
        const exact = event.pixelDelta.x !== 0 || event.pixelDelta.y !== 0;
        return exact ? Qt.point(event.pixelDelta.x, event.pixelDelta.y)
                     : Qt.point(event.angleDelta.x / 2, event.angleDelta.y / 2);
    }
}
