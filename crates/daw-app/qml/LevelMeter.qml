// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import org.kde.kirigami as Kirigami

// A vertical stereo peak meter, -60 to +6 dBFS. Readings fall back smoothly
// and the highest recent peak is held as a line for a moment.
Item {
    id: meter

    // Linear peaks since the last update.
    property real leftPeak: 0
    property real rightPeak: 0

    readonly property real floorDb: -60
    readonly property real topDb: 6

    implicitWidth: Kirigami.Units.smallSpacing * 4 + 1

    function position(linear: real): real {
        if (linear <= 0) {
            return 0;
        }
        const db = 20 * Math.log(linear) / Math.LN10;
        return Math.max(0, Math.min(1, (db - floorDb) / (topDb - floorDb)));
    }

    Accessible.role: Accessible.Indicator
    Accessible.name: i18nc("@label", "Level meter")

    Row {
        anchors.fill: parent
        spacing: 1
        Repeater {
            model: 2
            delegate: Rectangle {
                id: bar
                required property int index
                property real level: meter.position(index === 0 ? meter.leftPeak : meter.rightPeak)
                // Falls about 20 dB a second, rises at once.
                property real shown: 0
                property real held: 0
                property double heldAt: 0

                width: (meter.width - 1) / 2
                height: meter.height
                color: Kirigami.Theme.alternateBackgroundColor
                border.color: Kirigami.ColorUtils.linearInterpolation(Kirigami.Theme.backgroundColor, Kirigami.Theme.textColor, 0.15)
                border.width: 1

                onLevelChanged: {
                    shown = Math.max(level, shown - 0.01);
                    const now = Date.now();
                    if (shown >= held || now - heldAt > 1500) {
                        held = shown;
                        heldAt = now;
                    }
                }

                // The lit part: fixed colour zones, cut to the current level.
                // Green below -12 dB, amber up to 0 dB, red above.
                Item {
                    anchors {
                        left: parent.left
                        right: parent.right
                        bottom: parent.bottom
                        margins: 1
                    }
                    height: (bar.height - 2) * bar.shown
                    clip: true
                    Column {
                        anchors {
                            left: parent.left
                            right: parent.right
                            bottom: parent.bottom
                        }
                        readonly property real full: bar.height - 2
                        readonly property real span: meter.topDb - meter.floorDb
                        Rectangle {
                            width: parent.width
                            height: parent.full * meter.topDb / parent.span
                            color: Kirigami.Theme.negativeTextColor
                        }
                        Rectangle {
                            width: parent.width
                            height: parent.full * 12 / parent.span
                            color: Kirigami.Theme.neutralTextColor
                        }
                        Rectangle {
                            width: parent.width
                            height: parent.full * (-12 - meter.floorDb) / parent.span
                            color: Kirigami.Theme.positiveTextColor
                        }
                    }
                }
                Rectangle {
                    visible: bar.held > 0
                    x: 1
                    width: parent.width - 2
                    height: 1
                    y: 1 + (parent.height - 2) * (1 - bar.held)
                    color: bar.held > (-meter.floorDb) / (meter.topDb - meter.floorDb) ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                }
            }
        }
    }
}
