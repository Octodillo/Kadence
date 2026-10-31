// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtQml.Models

import io.github.octodillo.daw

// One channel strip on its own at a page's far side (the Edit page's
// track): the Mixer's row `row`, its line and width handle on the side
// facing the page's work. Empty while `row` is -1.
Item {
    id: track

    required property int row
    required property LayoutSize widthSize
    required property LayoutSize boxSize
    // The ChannelStrip, while there is one.
    readonly property Item strip: strips.count > 0 ? strips.itemAt(0) : null
    readonly property bool shown: strip !== null

    implicitWidth: shown ? widthSize.pixels : 0

    // The Mixer's rows, of which only `row` is made into a strip.
    DelegateModel {
        id: rows
        model: Mixer
        groups: DelegateModelGroup {
            id: chosen
            name: "chosen"
        }
        filterOnGroup: "chosen"
        delegate: ChannelStrip {
            height: track.height
            widthSize: track.widthSize
            boxSize: track.boxSize
            position: 0
            edge: Qt.LeftEdge
        }
    }

    // Puts the strip of `row`, and only it, in the "chosen" group. After a
    // reset the group can still hold the old strip, whose row is gone, so
    // it always starts again.
    function choose(): void {
        if (chosen.count > 0) {
            chosen.remove(0, chosen.count);
        }
        if (row >= 0 && row < rows.items.count) {
            rows.items.addGroups(row, 1, "chosen");
        }
    }
    onRowChanged: choose()
    Component.onCompleted: choose()
    // A reset (strips came or went) leaves the group holding the old strip,
    // and the DelegateModel refills its items in its own reset handler,
    // which runs after this one: choose again once that is done.
    Connections {
        target: Mixer
        function onModelReset() { Qt.callLater(track.choose); }
    }

    Repeater {
        id: strips
        model: rows
    }
}
