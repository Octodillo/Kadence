// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

import io.github.octodillo.daw

// A size the user gives part of the window, by dragging a SplitHandle. It
// is kept in grid units, so it suits any display scale, and remembered in
// kadencerc under [Layout] `key` once a drag ends.
QtObject {
    id: size

    required property string key
    required property real defaultUnits
    property real minimumUnits: 1
    property real maximumUnits: 1000

    property real units: clamp(AppSettings.layoutValue(key, defaultUnits))
    readonly property real pixels: units * Kirigami.Units.gridUnit
    readonly property real minimumPixels: minimumUnits * Kirigami.Units.gridUnit

    // The drag's start and the most it may grow to, in pixels.
    property real from: 0
    property real most: Infinity

    function clamp(units: real): real {
        return Math.max(minimumUnits, Math.min(maximumUnits, units));
    }

    // `start` is the size shown now (which a layout may have squeezed).
    function begin(start: real, limit: real): void {
        from = start;
        most = limit;
    }

    function drag(delta: real, done: bool): void {
        units = clamp(Math.min(from + delta, most) / Kirigami.Units.gridUnit);
        if (done) {
            AppSettings.setLayoutValue(key, units);
        }
    }

    function reset(): void {
        units = clamp(defaultUnits);
        AppSettings.setLayoutValue(key, units);
    }
}
